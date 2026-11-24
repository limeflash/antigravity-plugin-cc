//! The read-only agy run behind /agy:ask, /agy:scrape, /agy:doc-to-md and
//! the review commands.
//!
//! What keeps it read-only, all at once:
//! - agy's cwd and only `--add-dir` is a throwaway (or review-stage) dir, so
//!   the user's repo is outside agy's workspace;
//! - the run is pinned to agy's Default agent mode (`--settings
//!   {"agentMode":""}`, placed LAST so caller flags can't override it), so a
//!   headless run DENIES writes outside the workspace — agy >= 1.5.7 would
//!   otherwise start in Auto mode and approve them (verified on 1.5.16);
//! - no `--dangerously-skip-permissions`;
//! - repo-location hints are stripped from agy's environment.
//!
//! Output comes from `--output-format json` (`response`, `status`,
//! `denied_actions`), so a blocked write is reported as such instead of as
//! "no output".

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::agy::Agy;
use crate::json::{self, Value};
use crate::util::{self, Fail, TempDir};

/// Environment variables that point at the user's repo. agy runs outside
/// the repo; don't hand it the path either. PWD/OLDPWD matter because
/// `Command::current_dir` changes the cwd but not the inherited PWD.
pub(crate) const REPO_ENV_HINTS: &[&str] = &[
    "CLAUDE_PROJECT_DIR",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "AGY_JOB_WORKSPACE",
    "PWD",
    "OLDPWD",
];

/// agy takes the whole prompt as one argv entry; Windows caps a command line
/// at 32767 UTF-16 units. Leave room for the exe path and other flags.
pub(crate) const DEFAULT_PROMPT_MAX_BYTES: usize = 30_000;

/// Appended to every read-only prompt.
pub(crate) const READONLY_NOTE: &str = "\n\n(This is a read-only session: shell commands and file edits are not available and will be refused. Use only your built-in tools for reading files and listing directories.)";
const TRUNCATION_MARKER: &str = "\n\n[...content truncated to fit the OS command-line length limit; some context was omitted...]";

/// Where agy runs.
pub(crate) enum Workspace<'a> {
    /// A fresh private temp dir, optionally with one validated file copied in
    /// under a fixed name (doc-to-md).
    Fresh { stage: Option<(&'a Path, &'a str)> },
    /// An existing dir prepared by the caller (review stage dir).
    Existing(&'a Path),
}

pub(crate) struct Run<'a> {
    pub(crate) prompt: &'a str,
    pub(crate) model: Option<&'a str>,
    /// agy duration ("8m0s"), passed to --print-timeout.
    pub(crate) timeout: &'a str,
    /// Extra agy flags, already checked by the caller (see
    /// `check_passthrough`).
    pub(crate) extra: &'a [String],
    pub(crate) workspace: Workspace<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Denied {
    pub(crate) action: String,
    pub(crate) display_name: String,
}

#[derive(Debug)]
pub(crate) struct Outcome {
    pub(crate) response: String,
    /// agy's `status` ("SUCCESS", "ERROR", ...); None if stdout wasn't JSON.
    pub(crate) status: Option<String>,
    pub(crate) finished_reason: Option<String>,
    pub(crate) denied: Vec<Denied>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) stderr: String,
}

/// Flags a user may pass through to agy on a read-only command. Anything
/// that could widen what agy may touch (--add-dir, --mode, --settings,
/// --dangerously-skip-permissions, --remote-agent, ...) is refused.
const READONLY_PASSTHROUGH: &[&str] = &["--print-timeout", "--effort"];

/// Validate caller-supplied extra agy flags for a read-only command.
/// Accepts `--flag value` and `--flag=value` for allowed flags only.
pub(crate) fn check_passthrough(extra: &[String]) -> Result<(), Fail> {
    let refuse = |tok: &str| {
        Fail::usage(format!(
            "error: '{tok}' can't be passed to agy on a read-only command (allowed: {}, each with a value).",
            READONLY_PASSTHROUGH.join(", ")
        ))
    };
    let mut i = 0;
    while i < extra.len() {
        let tok = &extra[i];
        let (name, inline) = match tok.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (tok.as_str(), None),
        };
        if !READONLY_PASSTHROUGH.contains(&name) {
            return Err(refuse(tok));
        }
        // Each allowed flag takes a value. A missing value, or a "value"
        // that is itself a flag, would make agy consume the NEXT token —
        // possibly our own mode pin — as this flag's value.
        let value = match inline {
            Some(v) => v,
            None => {
                i += 1;
                extra.get(i).map(String::as_str).unwrap_or("")
            }
        };
        if value.is_empty() || value.starts_with('-') {
            return Err(refuse(tok));
        }
        i += 1;
    }
    Ok(())
}

/// Parse a Go duration like "8m0s", "90s", "1h30m", "500ms" into a Duration.
pub(crate) fn parse_go_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s == "0" {
        return Some(Duration::ZERO);
    }
    let mut total = 0f64;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
            i += 1;
        }
        let num: f64 = s[start..i].parse().ok()?;
        let ustart = i;
        while i < b.len() && b[i].is_ascii_alphabetic() {
            i += 1;
        }
        let mult = match &s[ustart..i] {
            "h" => 3600.0,
            "m" => 60.0,
            "s" => 1.0,
            "ms" => 0.001,
            "us" | "µs" => 0.000_001,
            "ns" => 0.000_000_001,
            _ => return None,
        };
        total += num * mult;
    }
    (total.is_finite() && total >= 0.0).then(|| Duration::from_secs_f64(total))
}

pub(crate) fn cap_prompt(prompt: &str) -> String {
    let max = util::env_nonempty("AGY_PROMPT_MAX_BYTES")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_PROMPT_MAX_BYTES);
    if prompt.len() <= max {
        return prompt.to_string();
    }
    format!("{}{TRUNCATION_MARKER}", util::truncate_utf8(prompt, max))
}

/// The argv after the agy binary (exposed for tests).
pub(crate) fn build_args(
    workspace: &Path,
    run: &Run,
    model: Option<&str>,
    mode_args: &[String],
    prompt: &str,
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "--sandbox".into(),
        "--add-dir".into(),
        workspace.display().to_string(),
        "--print-timeout".into(),
        run.timeout.into(),
        "--output-format".into(),
        "json".into(),
    ];
    if let Some(m) = model {
        a.push("--model".into());
        a.push(m.into());
    }
    a.extend(run.extra.iter().cloned());
    // LAST: agy takes the final value of a repeated flag.
    a.extend(mode_args.iter().cloned());
    a.push("--print".into());
    a.push(prompt.into());
    a
}

/// The --print-timeout agy will actually use: a caller-supplied one comes
/// after ours in argv, and agy takes the last value of a repeated flag.
pub(crate) fn effective_print_timeout<'a>(run: &'a Run) -> &'a str {
    let mut t = run.timeout;
    let mut i = 0;
    while i < run.extra.len() {
        let a = &run.extra[i];
        if a == "--print-timeout" {
            if let Some(v) = run.extra.get(i + 1) {
                t = v;
            }
            i += 1;
        } else if let Some(v) = a.strip_prefix("--print-timeout=") {
            t = v;
        }
        i += 1;
    }
    t
}

pub(crate) fn run_readonly(agy: &Agy, run: &Run) -> Result<Outcome, Fail> {
    agy.require_supported()?;
    let mode_args = agy.readonly_mode_args()?;

    let temp;
    let workspace: PathBuf = match &run.workspace {
        Workspace::Existing(p) => p.to_path_buf(),
        Workspace::Fresh { stage } => {
            temp = TempDir::new("agy-run-").map_err(|e| {
                Fail::runtime(format!("error: could not create a temp dir for agy: {e}"))
            })?;
            if let Some((src, name)) = stage {
                crate::guard::stage_file(src, &temp.path().join(name)).map_err(|e| {
                    Fail::runtime(format!("error: could not stage the input file: {e}"))
                })?;
            }
            temp.path().to_path_buf()
        }
    };

    // Tell agy up front which tools it has. In Default mode a shell command
    // or edit is refused, and agy 1.5.16 then tends to give up with an empty
    // answer instead of falling back to its read tools (seen live on
    // adversarial reviews).
    let prompt = format!("{}{READONLY_NOTE}", cap_prompt(run.prompt));
    let args = build_args(&workspace, run, run.model, &mode_args, &prompt);
    let mut cmd = agy.command();
    cmd.args(&args).current_dir(&workspace);
    for k in REPO_ENV_HINTS {
        cmd.env_remove(k);
    }
    // Our own hard stop a bit after agy's --print-timeout, in case agy hangs
    // on exit (it did before 1.5.5).
    let deadline = parse_go_duration(effective_print_timeout(run))
        .filter(|d| !d.is_zero())
        .map(|d| d + Duration::from_secs(120));
    let out =
        util::run_captured_notify(&mut cmd, deadline, &crate::jobs::note_child).map_err(|e| {
            Fail::runtime(format!(
                "error: could not run agy ({}): {e}",
                agy.path().display()
            ))
        })?;
    if out.timed_out {
        return Err(Fail::runtime(
            "error: agy did not finish within its timeout (raise it with the AGY_*_TIMEOUT variable).",
        ));
    }
    Ok(parse_outcome(&out.stdout_str(), out.code, out.stderr_str()))
}

/// Interpret agy's stdout. JSON when `--output-format json` worked; plain
/// text otherwise (treated as the answer).
pub(crate) fn parse_outcome(stdout: &str, exit_code: Option<i32>, stderr: String) -> Outcome {
    let parsed = json::parse(stdout.trim())
        .ok()
        .filter(|v| v.as_object().is_some());
    let Some(v) = parsed else {
        return Outcome {
            response: stdout.trim_end().to_string(),
            status: None,
            finished_reason: None,
            denied: Vec::new(),
            exit_code,
            stderr,
        };
    };
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let denied = v
        .get("denied_actions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|d| Denied {
                    action: d
                        .get("action")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string(),
                    display_name: d
                        .get("display_name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    Outcome {
        response: s("response").unwrap_or_default().trim_end().to_string(),
        status: s("status"),
        finished_reason: s("finished_reason"),
        denied,
        exit_code,
        stderr,
    }
}

fn describe(d: &Denied) -> String {
    if d.display_name.is_empty() || d.display_name == d.action {
        d.action.clone()
    } else {
        format!("{} ({})", d.display_name, d.action)
    }
}

/// agy's stderr for display: drop its hint to re-run with
/// --dangerously-skip-permissions — on a read-only command that advice is
/// exactly what must not happen, and Claude tends to follow such hints.
fn sanitized_stderr(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|l| !l.contains("dangerously-skip-permissions"))
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Print an outcome the way every read-only command does: the answer on
/// stdout, blocked actions as notes on stderr. Returns the exit code.
pub(crate) fn report(outcome: &Outcome) -> Result<i32, Fail> {
    for d in &outcome.denied {
        eprintln!(
            "[agy-plugin] note: agy tried to use {}, which this read-only command blocks.",
            describe(d)
        );
    }
    // JSON status when agy gave one; otherwise (plain-text output) its exit
    // code decides.
    let failed_status = match outcome.status.as_deref() {
        Some(s) => s != "SUCCESS",
        None => outcome.exit_code.is_some_and(|c| c != 0),
    };
    if !outcome.response.is_empty() {
        println!("{}", outcome.response);
        if let Some(reason) = &outcome.finished_reason {
            eprintln!(
                "[agy-plugin] note: agy's answer may be incomplete (finished_reason: {reason})."
            );
        }
        return Ok(if failed_status { 1 } else { 0 });
    }
    let mut msg = String::from("error: agy returned no answer.");
    if !outcome.denied.is_empty() {
        let list: Vec<String> = outcome.denied.iter().map(describe).collect();
        msg.push_str(&format!(
            "\n       It tried to use tools this read-only command blocks: {}.\n       In this read-only command agy can read only what the plugin handed it and can't change anything.",
            list.join(", ")
        ));
    }
    if let Some(st) = &outcome.status {
        msg.push_str(&format!("\n       agy status: {st}"));
    }
    if let Some(r) = &outcome.finished_reason {
        msg.push_str(&format!(" (finished_reason: {r})"));
    }
    let err = sanitized_stderr(&outcome.stderr);
    if !err.is_empty() {
        let tail: Vec<&str> = err
            .lines()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        msg.push_str("\n       agy said:");
        for l in tail {
            msg.push_str(&format!("\n         {l}"));
        }
    }
    msg.push_str(
        "\n       Run `agy` interactively to debug, or check ~/.gemini/antigravity-cli/log/.",
    );
    Err(Fail::runtime(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_print_timeout_wins_for_our_deadline() {
        let none: Vec<String> = vec![];
        let run = |extra| Run {
            prompt: "p",
            model: None,
            timeout: "8m0s",
            extra,
            workspace: Workspace::Fresh { stage: None },
        };
        assert_eq!(effective_print_timeout(&run(&none)), "8m0s");
        let e = vec!["--print-timeout".to_string(), "20m".to_string()];
        assert_eq!(effective_print_timeout(&run(&e)), "20m");
        let e = vec![
            "--print-timeout=1h".to_string(),
            "--effort".to_string(),
            "low".to_string(),
        ];
        assert_eq!(effective_print_timeout(&run(&e)), "1h");
    }

    #[test]
    fn go_durations() {
        assert_eq!(parse_go_duration("8m0s"), Some(Duration::from_secs(480)));
        assert_eq!(parse_go_duration("10m"), Some(Duration::from_secs(600)));
        assert_eq!(parse_go_duration("1h30m"), Some(Duration::from_secs(5400)));
        assert_eq!(parse_go_duration("1.5s"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_go_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_go_duration("0"), Some(Duration::ZERO));
        assert_eq!(parse_go_duration("5x"), None);
        assert_eq!(parse_go_duration(""), None);
        assert_eq!(parse_go_duration("m"), None);
    }

    #[test]
    fn passthrough_allows_only_safe_flags() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(check_passthrough(&v(&[])).is_ok());
        assert!(check_passthrough(&v(&["--print-timeout", "20m", "--effort=high"])).is_ok());
        for bad in [
            &["--dangerously-skip-permissions"][..],
            &["--mode", "auto"],
            &["--settings={\"agentMode\":\"auto\"}"],
            &["--add-dir", "/"],
            &["--remote-agent"],
            &["--print-timeout", "1m", "--mode=auto"],
            // A value-less allowed flag would swallow the next token (our pin).
            &["--effort"],
            &["--effort", "--settings={}"],
            &["--print-timeout="],
            &["--effort=", "high"],
        ] {
            assert_eq!(check_passthrough(&v(bad)).unwrap_err().code, 64, "{bad:?}");
        }
    }

    #[test]
    fn args_put_the_mode_pin_last_and_never_auto_approve() {
        let extra = vec!["--effort".to_string(), "high".to_string()];
        let run = Run {
            prompt: "p",
            model: None,
            timeout: "8m0s",
            extra: &extra,
            workspace: Workspace::Fresh { stage: None },
        };
        let mode = vec![format!("--settings={}", crate::agy::READ_ONLY_SETTINGS)];
        let a = build_args(
            Path::new("/tmp/ws"),
            &run,
            Some("gemini-3.8-flash-low"),
            &mode,
            "hello",
        );
        assert_eq!(
            a,
            vec![
                "--sandbox",
                "--add-dir",
                "/tmp/ws",
                "--print-timeout",
                "8m0s",
                "--output-format",
                "json",
                "--model",
                "gemini-3.8-flash-low",
                "--effort",
                "high",
                &mode[0],
                "--print",
                "hello",
            ]
        );
        assert!(!a.iter().any(|x| x.contains("dangerously")));
    }

    #[test]
    fn prompt_cap_keeps_utf8_valid() {
        let long = "é".repeat(DEFAULT_PROMPT_MAX_BYTES); // 2 bytes each
        let capped = cap_prompt(&long);
        assert!(capped.ends_with(TRUNCATION_MARKER));
        assert!(capped.len() <= DEFAULT_PROMPT_MAX_BYTES + TRUNCATION_MARKER.len());
        assert_eq!(cap_prompt("short"), "short");
    }

    #[test]
    fn parses_real_agy_json_results() {
        // agy 1.5.16, normal answer.
        let o = parse_outcome(
            r#"{"conversation_id":"0795","status":"SUCCESS","response":"OK\n","duration_seconds":3.0,"num_turns":1}"#,
            Some(0),
            String::new(),
        );
        assert_eq!(o.response, "OK");
        assert_eq!(o.status.as_deref(), Some("SUCCESS"));
        assert!(o.denied.is_empty());
        // agy 1.5.16, write blocked by Default mode (from the live probe).
        let o = parse_outcome(
            r#"{"status":"SUCCESS","response":"","denied_actions":[{"action":"write_file","display_name":"Edit"}]}"#,
            Some(0),
            "jetski: no output produced — ... re-run with --dangerously-skip-permissions to auto-approve all tools.\n".into(),
        );
        assert_eq!(
            o.denied,
            vec![Denied {
                action: "write_file".into(),
                display_name: "Edit".into()
            }]
        );
        let err = report(&o).unwrap_err();
        assert!(err.message.contains("Edit (write_file)"));
        assert!(
            !err.message.contains("dangerously"),
            "must not echo the skip-permissions hint"
        );
    }

    #[test]
    fn plain_text_stdout_is_the_answer() {
        let o = parse_outcome("42\n", Some(0), String::new());
        assert_eq!(o.response, "42");
        assert_eq!(o.status, None);
    }

    #[test]
    fn error_status_with_partial_answer_exits_1() {
        let o = parse_outcome(
            r#"{"status":"ERROR","response":"partial","finished_reason":"print_timeout"}"#,
            Some(3),
            String::new(),
        );
        assert_eq!(report(&o).unwrap(), 1);
    }
}
