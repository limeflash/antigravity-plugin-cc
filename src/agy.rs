//! Locating and probing Google's `agy` CLI: binary lookup, auth probe,
//! version gates, and the flags that keep a read-only run read-only.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::util::{self, Fail};

/// Oldest agy this plugin drives. 1.2.0 is the first release where all of
/// these hold: non-TTY stdout carries the answer (the 1.0.15 fix for agy
/// issue #76), `--output-format json` works in print mode, `--model` is
/// honoured in `-p` runs (1.1.10) and `agy models` doesn't hang.
pub(crate) const MIN_AGY_VERSION: &str = "1.2.0";

/// agy 1.5.7 made Auto the default Agent Mode for headless `-p` runs. Auto
/// approves writes OUTSIDE the workspace without asking (1.5.16: any file
/// inside a git/jj/hg repo, and any create or append elsewhere). Verified on
/// 1.5.16: a plugin-style run with no --dangerously-skip-permissions
/// overwrote a file in a git repo outside its temp workspace.
pub(crate) const AGY_AUTO_DEFAULT_VERSION: &str = "1.5.7";

/// Session-only settings selecting agy's Default agent mode, in which a
/// headless run DENIES anything that would need approval (reported as
/// `denied_actions`). Passed with --settings, never saved.
///
/// The deny rules are a second, independent layer (verified on agy 1.5.16:
/// a session `--settings` deny rule blocks the action even in Auto mode, and
/// a deny beats any allow rule the user saved): `command(*)` refuses every
/// shell command, `write_file(*)` every file creation or edit. In Default
/// mode agy also refuses to READ files outside its workspace.
pub(crate) const READ_ONLY_SETTINGS: &str =
    r#"{"agentMode":"","permissions":{"deny":["command(*)","write_file(*)"]}}"#;

pub(crate) const INSTALL_HINT: &str = "curl -fsSL https://antigravity.google/cli/install.sh | bash";

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Compare dotted versions numerically ("1.10" > "1.9"); missing or
/// non-numeric parts count as 0.
pub(crate) fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let part = |s: &str, i: usize| -> u64 {
        s.split('.')
            .filter(|p| !p.is_empty())
            .nth(i)
            .map(|p| {
                let digits: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
                digits.parse().unwrap_or(0)
            })
            .unwrap_or(0)
    };
    let n = a.split('.').count().max(b.split('.').count());
    for i in 0..n {
        match part(a, i).cmp(&part(b, i)) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

pub(crate) fn version_at_least(version: &str, min: &str) -> bool {
    compare_versions(version, min) != std::cmp::Ordering::Less
}

/// First `N.N` or `N.N.N` token in `text` ("agy 1.5.16" -> "1.5.16").
pub(crate) fn parse_version(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !b[i - 1].is_ascii_digit()) {
            let mut j = i;
            let mut parts = 0;
            loop {
                let start = j;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                if j == start {
                    break;
                }
                parts += 1;
                if parts == 3
                    || j >= b.len()
                    || b[j] != b'.'
                    || j + 1 >= b.len()
                    || !b[j + 1].is_ascii_digit()
                {
                    break;
                }
                j += 1;
            }
            if parts >= 2 {
                return Some(text[i..j].to_string());
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

/// Where agy is, in the order the 0.8.x wrappers searched: $AGY_BIN, PATH,
/// then the installer's default locations.
pub(crate) fn find_agy() -> Option<PathBuf> {
    if let Some(p) = util::env_nonempty("AGY_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(p) = util::which("agy") {
        return Some(p);
    }
    let home = util::home_dir();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(h) = &home {
        candidates.push(h.join(".local").join("bin").join("agy"));
    }
    candidates.push(PathBuf::from("/opt/antigravity/bin/agy"));
    candidates.push(PathBuf::from("/usr/local/bin/agy"));
    let local_app_data = util::env_nonempty("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join("AppData").join("Local")));
    if let Some(l) = local_app_data {
        candidates.push(l.join("agy").join("bin").join("agy.exe"));
    }
    candidates.into_iter().find(|c| {
        // On Windows an extension-less file (a POSIX script) can't be run.
        let runnable =
            !cfg!(windows) || c.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"));
        runnable && util::is_executable_file(c)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Auth {
    ApiKey,
    Oauth,
    Missing,
}

impl Auth {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Auth::ApiKey => "api-key",
            Auth::Oauth => "oauth",
            Auth::Missing => "missing",
        }
    }
}

/// Coarse, offline auth probe: an API key in the environment, or agy's
/// Oauth state directory on disk. It can't tell an expired login from a
/// valid one; /agy:setup's real ping does that.
pub(crate) fn auth_status() -> Auth {
    if util::env_nonempty("ANTIGRAVITY_API_KEY").is_some() {
        return Auth::ApiKey;
    }
    match util::home_dir() {
        Some(h)
            if h.join(".config").join("antigravity").is_dir()
                || h.join(".gemini").join("antigravity-cli").is_dir() =>
        {
            Auth::Oauth
        }
        _ => Auth::Missing,
    }
}

/// A located agy binary with lazily probed `--version` / `--help`.
pub(crate) struct Agy {
    path: PathBuf,
    version: OnceCell<Option<String>>,
    help: OnceCell<String>,
}

impl Agy {
    pub(crate) fn at(path: PathBuf) -> Agy {
        Agy {
            path,
            version: OnceCell::new(),
            help: OnceCell::new(),
        }
    }

    /// Find agy and check it is logged in (exit 127 / 1 like 0.8.x).
    pub(crate) fn ready() -> Result<Agy, Fail> {
        let Some(path) = find_agy() else {
            return Err(Fail::new(
                127,
                format!("error: agy is not installed.\n       install: {INSTALL_HINT}"),
            ));
        };
        if auth_status() == Auth::Missing {
            return Err(Fail::runtime(
                "error: agy is not authenticated.\n       run `agy` once interactively, or export ANTIGRAVITY_API_KEY",
            ));
        }
        Ok(Agy::at(path))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn command(&self) -> Command {
        Command::new(&self.path)
    }

    pub(crate) fn version(&self) -> Option<&str> {
        self.version
            .get_or_init(|| {
                let out = util::run_simple(&self.path, &["--version"], PROBE_TIMEOUT).ok()?;
                if !out.ok() {
                    return None;
                }
                parse_version(&out.stdout_str())
            })
            .as_deref()
    }

    /// `agy --help` text (stdout + stderr; Go's flag package prints usage to
    /// stderr on some builds). Empty if agy can't be run.
    pub(crate) fn help(&self) -> &str {
        self.help.get_or_init(
            || match util::run_simple(&self.path, &["--help"], PROBE_TIMEOUT) {
                Ok(out) => format!("{}\n{}", out.stdout_str(), out.stderr_str()),
                Err(_) => String::new(),
            },
        )
    }

    fn has_flag(&self, flag: &str) -> bool {
        // Match the flag as a whole word in the usage table, not a substring
        // of a longer flag (`--model` must not match `--models-dir`).
        self.help().lines().any(|l| {
            l.split(|c: char| c.is_whitespace() || c == ',' || c == '=')
                .any(|w| w == flag)
        })
    }

    /// Refuse agy builds older than MIN_AGY_VERSION. An unreadable version
    /// is accepted when the help text shows the JSON output flag we rely on.
    pub(crate) fn require_supported(&self) -> Result<(), Fail> {
        match self.version() {
            Some(v) if !version_at_least(v, MIN_AGY_VERSION) => Err(Fail::runtime(format!(
                "error: agy {v} is too old for this plugin (needs {MIN_AGY_VERSION} or newer).\n       update it with `agy update`."
            ))),
            Some(_) => Ok(()),
            None if self.has_flag("--output-format") => Ok(()),
            None => Err(Fail::runtime(format!(
                "error: could not determine the agy version, and `agy --help` does not list --output-format.\n       this plugin needs agy {MIN_AGY_VERSION} or newer; update it with `agy update`."
            ))),
        }
    }

    /// Flags that pin a read-only run to Default agent mode. Fails closed:
    /// an agy that starts headless runs in Auto mode but has no --settings
    /// flag is refused rather than run.
    pub(crate) fn readonly_mode_args(&self) -> Result<Vec<String>, Fail> {
        if self.has_flag("--settings") {
            // ONE token (`--settings=<json>`): a separate value token could
            // be swallowed by a preceding value-less flag (`--effort
            // --settings {...}` would make "--settings" the effort value).
            return Ok(vec![format!("--settings={READ_ONLY_SETTINGS}")]);
        }
        match self.version() {
            Some(v) if version_at_least(v, AGY_AUTO_DEFAULT_VERSION) => {
                Err(Fail::runtime(format!(
                    "error: agy {v} starts headless runs in Auto mode, which can write outside\n       the workspace, and this build has no --settings flag to pin Default mode.\n       Refusing to run a read-only command. Update agy with `agy update`."
                )))
            }
            // Older builds predate Auto mode: Default is the only mode.
            _ => Ok(Vec::new()),
        }
    }
}

#[cfg(all(test, unix))]
pub(crate) mod test_support {
    //! A scripted fake `agy` for unit tests (Unix shell script).
    use std::path::{Path, PathBuf};

    /// Write an executable stub at dir/agy that prints `version` for
    /// --version and `help` for --help.
    #[cfg(unix)]
    pub(crate) fn stub_agy(dir: &Path, version: &str, help: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("agy");
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) printf '%s\\n' '{version}' ;;\n  --help) cat <<'EOF'\n{help}\nEOF\n;;\nesac\n"
        );
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn version_comparison_is_numeric() {
        assert_eq!(compare_versions("1.10.0", "1.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.2", "1.2.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.5.16", "1.5.7"), Ordering::Greater);
        assert_eq!(compare_versions("1.1.5", "1.2.0"), Ordering::Less);
        assert_eq!(compare_versions("2", "1.99.99"), Ordering::Greater);
        assert_eq!(compare_versions("1.5-beta", "1.5"), Ordering::Equal);
        assert!(version_at_least("1.5.7", AGY_AUTO_DEFAULT_VERSION));
        assert!(!version_at_least("1.5.6", AGY_AUTO_DEFAULT_VERSION));
    }

    #[test]
    fn parse_version_finds_the_first_dotted_number() {
        assert_eq!(parse_version("1.5.16\n").as_deref(), Some("1.5.16"));
        assert_eq!(
            parse_version("agy version 1.2.0 (abc)").as_deref(),
            Some("1.2.0")
        );
        assert_eq!(parse_version("v1.4").as_deref(), Some("1.4"));
        assert_eq!(parse_version("1.2.3.4").as_deref(), Some("1.2.3"));
        assert_eq!(parse_version("build 42 of 7"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("1."), None);
    }

    #[cfg(unix)]
    fn agy_with(version: &str, help: &str) -> (crate::util::TempDir, Agy) {
        let d = crate::util::TempDir::new("agy-probe-test-").unwrap();
        let p = test_support::stub_agy(d.path(), version, help);
        (d, Agy::at(p))
    }

    #[cfg(unix)]
    #[test]
    fn readonly_pin_uses_settings_when_available() {
        let (_d, a) = agy_with("1.5.16", "  --settings  Settings for this session only");
        assert_eq!(
            a.readonly_mode_args().unwrap(),
            vec![format!("--settings={READ_ONLY_SETTINGS}")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn readonly_pin_refuses_auto_by_default_agy_without_settings() {
        let (_d, a) = agy_with("1.5.9", "Usage of agy:\n  --print  x");
        let err = a.readonly_mode_args().unwrap_err();
        assert_eq!(err.code, 1);
        assert!(err.message.contains("Refusing to run a read-only command"));
    }

    #[cfg(unix)]
    #[test]
    fn readonly_pin_is_empty_for_agy_that_predates_auto_mode() {
        let (_d, a) = agy_with("1.2.0", "Usage of agy:");
        assert!(a.readonly_mode_args().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn settings_must_match_as_a_whole_flag() {
        let (_d, a) = agy_with("1.5.16", "  --settings-file  something else");
        assert!(a.readonly_mode_args().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn version_gate() {
        let (_d, old) = agy_with("1.1.5", "");
        assert!(
            old.require_supported()
                .unwrap_err()
                .message
                .contains("too old")
        );
        let (_d2, new) = agy_with("1.5.16", "");
        assert!(new.require_supported().is_ok());
        let (_d3, unknown_ok) = agy_with("unknown", "  --output-format  Output format");
        assert!(unknown_ok.require_supported().is_ok());
        let (_d4, unknown_bad) = agy_with("unknown", "Usage:");
        assert!(unknown_bad.require_supported().is_err());
    }
}
