//! agy-plugin — Claude Code plugin runtime for Google's Antigravity CLI.
//!
//! One std-only binary behind the /agy:* slash commands. Read-only commands
//! (ask, review, adversarial-review, scrape, doc-to-md) run agy from a
//! throwaway workspace, pinned to its Default agent mode, so it can read but
//! not write; rescue and image are the only write-capable commands.

mod agy;
mod capture;
mod cli;
mod commands;
mod fetch;
mod git;
mod guard;
mod jobs;
mod json;
mod models;
mod prompts;
mod registry;
mod render;
mod secrets;
mod sys;
mod time;
mod util;

use std::path::PathBuf;

/// Plugin <= 0.8.x switched models by patching agy's settings.json under a
/// lock, with a backup + sentinel. If such a run was killed mid-way, put the
/// user's settings back (once; harmless when nothing is left over).
fn restore_orphaned_settings() {
    let Some(home) = util::home_dir() else { return };
    let dir: PathBuf = home.join(".gemini").join("antigravity-cli");
    let settings = dir.join("settings.json");
    let backup = dir.join("settings.json.agy-plugin.bak");
    let sentinel = dir.join(".agy-plugin.patched");
    let lock = dir.join(".agy-plugin.lock");
    if !sentinel.exists() {
        let _ = std::fs::remove_file(&backup);
        return;
    }
    let pid: Option<u32> = std::fs::read_to_string(&sentinel)
        .ok()
        .and_then(|s| s.lines().next().and_then(|l| l.trim().parse().ok()));
    if pid.is_some_and(sys::process_alive) {
        return; // an old-version run still owns it
    }
    if backup.exists() && std::fs::rename(&backup, &settings).is_ok() {
        eprintln!(
            "[agy-plugin] restored agy settings.json left patched by an interrupted older plugin run"
        );
    }
    let _ = std::fs::remove_file(&sentinel);
    let _ = std::fs::remove_dir_all(&lock);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = match args.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => ("help", &[][..]),
    };
    if cmd != "_run-job" {
        restore_orphaned_settings();
    }
    let result = match cmd {
        "check" => commands::check(),
        "ask" => commands::ask(rest),
        "review" => commands::review(rest, false),
        "adversarial-review" => commands::review(rest, true),
        "scrape" => commands::scrape(rest),
        "doc-to-md" => commands::doc_to_md(rest),
        "image" => commands::image(rest),
        "rescue" => commands::rescue(rest),
        "status" => commands::status(rest),
        "result" => commands::result(rest),
        "cancel" => commands::cancel(rest),
        "version" | "--version" => commands::version(),
        "help" | "-h" | "--help" => commands::help(),
        "_run-job" => commands::run_job(rest),
        other => Err(util::Fail::usage(format!(
            "error: unknown subcommand '{other}' (see `agy-plugin help`)"
        ))),
    };
    let code = match result {
        Ok(c) => c,
        Err(f) => {
            if !f.message.is_empty() {
                eprintln!("{}", f.message);
            }
            f.code
        }
    };
    std::process::exit(code);
}
