//! A scripted stand-in for Google's `agy`, used by tests/cli.rs (std only).
//!
//! Every invocation appends one line to $STUB_RECORD:
//!   <cwd>\t<argv joined by \x1f>\t<env KEY=VALUE pairs joined by \x1f>
//! (env limited to the variables the plugin is supposed to strip, so tests
//! can assert they are gone).
//!
//! Behaviour by argv:
//!   --version          prints $STUB_VERSION (default 1.5.16)
//!   --help             prints $STUB_HELP (default: lists --model, --settings, --output-format)
//!   models             prints "Fetching..." + $STUB_MODELS (exit 1 if $STUB_MODELS_FAIL=1)
//!   anything with --print / -p:
//!       sleeps $STUB_SLEEP_MS, writes the prompt to $STUB_PROMPT_OUT,
//!       if $STUB_MARKER_FROM_PROMPT=1 writes $STUB_IMAGE into the marker path named in the prompt,
//!       prints a JSON result when --output-format json was given
//!       ({"status":$STUB_STATUS,"response":$STUB_RESPONSE,"denied_actions":[...$STUB_DENIED]}),
//!       else $STUB_RESPONSE; exits $STUB_EXIT (default 0).

use std::io::Write;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

const WATCHED_ENV: &[&str] = &[
    "CLAUDE_PROJECT_DIR",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "AGY_JOB_WORKSPACE",
    "PWD",
    "OLDPWD",
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(rec) = env("STUB_RECORD") {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let envs: Vec<String> = WATCHED_ENV
            .iter()
            .filter_map(|k| env(k).map(|v| format!("{k}={v}")))
            .collect();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(rec)
        {
            let _ = writeln!(f, "{cwd}\t{}\t{}", args.join("\x1f"), envs.join("\x1f"));
        }
    }
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("{}", env("STUB_VERSION").unwrap_or_else(|| "1.5.16".into()));
            return;
        }
        Some("--help") => {
            println!(
                "{}",
                env("STUB_HELP").unwrap_or_else(|| "Usage of agy:\n  --model  Model\n  --output-format  Output format\n  --settings  Settings for this session only".into())
            );
            return;
        }
        Some("models") => {
            if env("STUB_MODELS_FAIL").as_deref() == Some("1") {
                std::process::exit(1);
            }
            println!("Fetching available models...");
            print!(
                "{}",
                env("STUB_MODELS")
                    .unwrap_or_default()
                    .replace("\\t", "\t")
                    .replace("\\n", "\n")
            );
            return;
        }
        _ => {}
    }
    let prompt = args
        .iter()
        .position(|a| a == "--print" || a == "-p")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    if let Some(p) = env("STUB_PID_OUT") {
        let _ = std::fs::write(p, std::process::id().to_string());
    }
    if let Some(ms) = env("STUB_SLEEP_MS").and_then(|v| v.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    if let Some(p) = env("STUB_PROMPT_OUT") {
        let _ = std::fs::write(p, &prompt);
    }
    if env("STUB_MARKER_FROM_PROMPT").as_deref() == Some("1")
        && let Some(marker) = prompt.lines().find(|l| l.ends_with("image-path.txt"))
    {
        let _ = std::fs::write(marker.trim(), env("STUB_IMAGE").unwrap_or_default());
    }
    let response = env("STUB_RESPONSE").unwrap_or_default();
    if args
        .windows(2)
        .any(|w| w[0] == "--output-format" && w[1] == "json")
    {
        let denied: Vec<String> = env("STUB_DENIED")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|a| format!("{{\"action\":{},\"display_name\":\"Edit\"}}", json_str(a)))
            .collect();
        println!(
            "{{\"conversation_id\":\"c\",\"status\":{},\"response\":{},\"denied_actions\":[{}]}}",
            json_str(&env("STUB_STATUS").unwrap_or_else(|| "SUCCESS".into())),
            json_str(&response),
            denied.join(",")
        );
    } else {
        println!("{response}");
    }
    if let Some(e) = env("STUB_STDERR") {
        eprintln!("{e}");
    }
    std::process::exit(env("STUB_EXIT").and_then(|v| v.parse().ok()).unwrap_or(0));
}
