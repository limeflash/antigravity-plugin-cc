//! Small std-only helpers: temp dirs, randomness, PATH lookup, env, and
//! running a child process with captured output and a deadline.

use std::collections::hash_map::RandomState;
use std::ffi::OsStr;
use std::hash::{BuildHasher, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 64 bits from the OS RNG. std's `RandomState` seeds every instance from the
/// operating system's random source, which is all we need for job ids and
/// temp-dir names (unpredictable, not cryptographic keys).
pub(crate) fn random_u64() -> u64 {
    let mut h = RandomState::new().build_hasher();
    h.write_u64(0x9e37_79b9_7f4a_7c15);
    h.finish()
}

pub(crate) fn random_hex(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes * 2);
    let mut left = bytes;
    while left > 0 {
        let chunk = random_u64().to_le_bytes();
        for b in chunk.iter().take(left.min(8)) {
            out.push_str(&format!("{b:02x}"));
        }
        left = left.saturating_sub(8);
    }
    out
}

/// Remove our own temp dirs (`<prefix>*` directly in the OS temp dir, not
/// symlinks) older than `max_age` — left behind when a run was killed
/// before its cleanup (e.g. Claude's Bash tool timing out a foreground
/// review). They can hold full copies of reviewed source files.
pub(crate) fn sweep_stale_temp(prefixes: &[&str], max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if !prefixes.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(e.path()) else {
            continue;
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            continue;
        }
        let old = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > max_age);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// A directory under the OS temp dir that is removed when dropped (unless
/// `keep()` was called). Created with mode 0700 on Unix, so other local users
/// can't read staged diffs or plant files into agy's workspace.
pub(crate) struct TempDir {
    path: PathBuf,
    keep: bool,
}

impl TempDir {
    pub(crate) fn new(prefix: &str) -> std::io::Result<TempDir> {
        let base = std::env::temp_dir();
        let mut last_err = None;
        for _ in 0..16 {
            let path = base.join(format!("{prefix}{}", random_hex(6)));
            #[cfg_attr(not(unix), allow(unused_mut))] // mode() is Unix-only
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => {
                    // Hand out the canonical path. On macOS the temp dir is
                    // /var/folders/..., a symlink to /private/var/...: agy's
                    // cwd resolves to the /private form, and its --add-dir
                    // must name the same spelling or agy may judge its own
                    // workspace to be outside the workspace.
                    let path = canonical_or_self(&path);
                    return Ok(TempDir { path, keep: false });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last_err = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| std::io::Error::other("could not create a temp dir")))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Hand ownership of the directory to someone else (e.g. a background
    /// worker that deletes it when it finishes).
    pub(crate) fn keep(mut self) -> PathBuf {
        self.keep = true;
        self.path.clone()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// The value of an environment variable if it is set and non-empty.
pub(crate) fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The user's home directory. HOME first on Unix (that's what the old shell
/// code honoured, and tests override it); USERPROFILE on Windows (Git Bash's
/// HOME is an MSYS path like /c/Users/x that native programs can't open).
pub(crate) fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let var = env_nonempty("USERPROFILE").or_else(|| env_nonempty("HOME"));
    #[cfg(not(windows))]
    let var = env_nonempty("HOME");
    var.map(PathBuf::from).or_else(std::env::home_dir)
}

pub(crate) fn is_executable_file(p: &Path) -> bool {
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                m.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                true
            }
        }
        _ => false,
    }
}

/// Find `name` on PATH like a shell would (PATHEXT on Windows).
pub(crate) fn which(name: &str) -> Option<PathBuf> {
    which_in(name, &std::env::var_os("PATH")?)
}

fn which_in(name: &str, path: &OsStr) -> Option<PathBuf> {
    // Windows: only real executable extensions (an extension-less file is
    // never runnable there, e.g. a POSIX `agy` script left in ~/.local/bin).
    #[cfg(windows)]
    let exts: Vec<String> = env_nonempty("PATHEXT")
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    #[cfg(not(windows))]
    let exts: Vec<String> = vec![String::new()];
    for dir in std::env::split_paths(path) {
        // Skip empty and relative entries (`.`, `bin`): they resolve against
        // the current directory, i.e. whatever repo the user is in, so a
        // repo could plant its own `agy`, `git` or `curl`.
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            continue;
        }
        for ext in &exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Output of a finished (or killed) child process.
pub(crate) struct Captured {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    /// None when killed by a signal or by our deadline.
    pub(crate) code: Option<i32>,
    pub(crate) timed_out: bool,
}

impl Captured {
    pub(crate) fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
    pub(crate) fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
    pub(crate) fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

/// Run `cmd` with stdin closed (agy blocks forever on an open non-TTY stdin),
/// capturing stdout and stderr, and kill it after `timeout` if given.
/// Spawn failures are returned as Err.
pub(crate) fn run_captured(
    cmd: &mut Command,
    timeout: Option<Duration>,
) -> std::io::Result<Captured> {
    run_captured_notify(cmd, timeout, &|_| {})
}

/// `run_captured`, telling `on_spawn` the child's pid as soon as it starts
/// (jobs record agy's pid so /agy:cancel can stop it).
pub(crate) fn run_captured_notify(
    cmd: &mut Command,
    timeout: Option<Duration>,
    on_spawn: &dyn Fn(u32),
) -> std::io::Result<Captured> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    on_spawn(child.id());
    // Drain both pipes on threads: a child that fills one pipe while we wait
    // on the other would deadlock.
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = timeout.map(|t| Instant::now() + t);
    let mut timed_out = false;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break Some(st);
        }
        if let Some(d) = deadline
            && Instant::now() >= d
        {
            timed_out = true;
            let _ = child.kill();
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    let code = if timed_out {
        None
    } else {
        status.and_then(|s| s.code())
    };
    Ok(Captured {
        stdout,
        stderr,
        code,
        timed_out,
    })
}

/// Convenience: run `program args...` with a deadline.
pub(crate) fn run_simple<S: AsRef<OsStr>>(
    program: &Path,
    args: &[S],
    timeout: Duration,
) -> std::io::Result<Captured> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    run_captured(&mut cmd, Some(timeout))
}

/// Truncate to at most `max` bytes without splitting a UTF-8 character.
pub(crate) fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_hex_has_requested_length_and_varies() {
        let a = random_hex(4);
        let b = random_hex(4);
        assert_eq!(a.len(), 8);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(random_hex(11).len(), 22);
        assert_ne!(a, b, "two draws from the OS RNG should differ");
    }

    #[test]
    fn temp_dir_is_private_and_removed_on_drop() {
        let p;
        {
            let t = TempDir::new("agy-test-").unwrap();
            p = t.path().to_path_buf();
            assert!(p.is_dir());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o700);
            }
            std::fs::write(p.join("f"), "x").unwrap();
        }
        assert!(!p.exists());
    }

    #[test]
    fn temp_dir_keep_survives_drop() {
        let t = TempDir::new("agy-test-").unwrap();
        let p = t.keep();
        assert!(p.is_dir());
        std::fs::remove_dir_all(&p).unwrap();
    }

    #[test]
    fn truncate_utf8_respects_char_boundaries() {
        assert_eq!(truncate_utf8("hello", 10), "hello");
        assert_eq!(truncate_utf8("hello", 3), "hel");
        // "é" is 2 bytes; cutting in its middle must back off.
        assert_eq!(truncate_utf8("aé", 2), "a");
        assert_eq!(truncate_utf8("", 0), "");
    }

    #[cfg(unix)]
    #[test]
    fn run_captured_collects_both_streams_and_exit_code() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo out; echo err >&2; exit 3"]);
        let c = run_captured(&mut cmd, Some(Duration::from_secs(10))).unwrap();
        assert_eq!(c.stdout_str(), "out\n");
        assert_eq!(c.stderr_str(), "err\n");
        assert_eq!(c.code, Some(3));
        assert!(!c.timed_out);
    }

    #[cfg(unix)]
    #[test]
    fn run_captured_closes_stdin() {
        // `cat` would block forever on an open stdin; closed stdin -> EOF.
        let mut cmd = Command::new("cat");
        let c = run_captured(&mut cmd, Some(Duration::from_secs(10))).unwrap();
        assert!(c.ok());
        assert!(c.stdout.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn run_captured_kills_on_timeout() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let start = Instant::now();
        let c = run_captured(&mut cmd, Some(Duration::from_millis(200))).unwrap();
        assert!(c.timed_out);
        assert_eq!(c.code, None);
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn run_captured_reports_spawn_failure() {
        let mut cmd = Command::new("/definitely/not/a/real/binary-xyz");
        assert!(run_captured(&mut cmd, Some(Duration::from_secs(1))).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn which_ignores_relative_path_entries() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new("agy-which-test-").unwrap();
        let bin = d.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let tool = bin.join("agy-which-probe");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Absolute entry: found.
        assert_eq!(
            which_in("agy-which-probe", bin.as_os_str()),
            Some(tool.clone())
        );
        // The same dir written relatively (as a repo could plant): ignored.
        let rel = std::ffi::OsString::from("bin:.:");
        let cwd = std::env::current_dir().unwrap();
        assert!(cwd != d.path(), "test must not depend on cwd");
        assert_eq!(which_in("agy-which-probe", &rel), None);
    }

    #[cfg(unix)]
    #[test]
    fn which_finds_sh_and_not_garbage() {
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-command-xyz").is_none());
    }
}

/// A command failure: the exit code the process should end with and the
/// message for stderr (already formatted, may span lines).
#[derive(Debug)]
pub(crate) struct Fail {
    pub(crate) code: i32,
    pub(crate) message: String,
}

impl Fail {
    pub(crate) fn new(code: i32, message: impl Into<String>) -> Fail {
        Fail {
            code,
            message: message.into(),
        }
    }
    /// Bad usage (exit 64, like sysexits EX_USAGE).
    pub(crate) fn usage(message: impl Into<String>) -> Fail {
        Fail::new(64, message)
    }
    /// Refused by a safety guard (exit 65, EX_DATAERR — kept from 0.8.x).
    pub(crate) fn refused(message: impl Into<String>) -> Fail {
        Fail::new(65, message)
    }
    /// Environment / runtime failure (exit 1).
    pub(crate) fn runtime(message: impl Into<String>) -> Fail {
        Fail::new(1, message)
    }
}

pub(crate) type CmdResult = Result<i32, Fail>;

/// `std::fs::canonicalize` without Windows' verbatim `\\?\` prefix (agy, git
/// and many Windows tools reject verbatim paths); falls back to the input.
pub(crate) fn canonical_or_self(p: &Path) -> PathBuf {
    match std::fs::canonicalize(p) {
        Ok(c) => {
            let s = c.to_string_lossy();
            if let Some(rest) = s.strip_prefix(r"\\?\")
                && !rest.starts_with("UNC\\")
            {
                return PathBuf::from(rest);
            }
            c
        }
        Err(_) => p.to_path_buf(),
    }
}
