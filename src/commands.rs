//! The /agy:* commands.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::agy::{self, Agy};
use crate::capture::{self, Run, Workspace};
use crate::cli::{self, Schema};
use crate::jobs::{self, CancelResult, NewJob, Resolved, Sink};
use crate::json::{self, Map, Value};
use crate::util::{self, CmdResult, Fail, TempDir};

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// agy flags that take a value; listed so the parser forwards `--flag V`
/// as two tokens instead of eating the next word of a task.
const AGY_VALUE_FLAGS: &[&str] = &[
    "print-timeout",
    "effort",
    "add-dir",
    "agent",
    "mode",
    "settings",
    "log-file",
    "conversation",
    "resume",
    "project",
    "json-schema",
    "output-format",
    "input-format",
    "model",
];

fn timeout_env(var: &str, default: &str) -> String {
    match util::env_nonempty(var) {
        Some(v) if capture::parse_go_duration(&v).is_some() => v,
        Some(v) => {
            eprintln!(
                "[agy-plugin] note: ignoring {var}={v} (not a duration like 8m0s); using {default}"
            );
            default.to_string()
        }
        None => default.to_string(),
    }
}

/// The workspace a command operates on: $CLAUDE_PROJECT_DIR if it is a
/// directory, else the nearest ancestor with a `.git`, else the cwd.
pub(crate) fn workspace_root() -> PathBuf {
    if let Some(d) = util::env_nonempty("CLAUDE_PROJECT_DIR") {
        let p = PathBuf::from(d);
        if p.is_dir() {
            return std::path::absolute(&p).unwrap_or(p);
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut cur: Option<&Path> = Some(&cwd);
    while let Some(d) = cur {
        if d.join(".git").exists() {
            return d.to_path_buf();
        }
        cur = d.parent();
    }
    cwd
}

fn usage_errors(errors: &[String]) -> Fail {
    Fail::usage(
        errors
            .iter()
            .map(|e| format!("error: {e}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Leading `--model` handling for the single-target commands (ask, scrape,
/// doc-to-md): returns (model value if given, remaining args).
fn take_model(args: &[String]) -> Result<(Option<String>, Vec<String>), Fail> {
    let mut model = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--model" {
            let v = args.get(i + 1).cloned().unwrap_or_default();
            model = Some(v);
            i += 2;
        } else if let Some(v) = a.strip_prefix("--model=") {
            model = Some(v.to_string());
            i += 1;
        } else if a == "--" {
            i += 1;
            break;
        } else {
            break;
        }
    }
    if model.as_deref().is_some_and(|m| m.trim().is_empty()) {
        return Err(crate::models::empty_value_error(
            agy::find_agy().map(Agy::at).as_ref(),
        ));
    }
    Ok((model, args[i.min(args.len())..].to_vec()))
}

fn resolve_model(agy: &Agy, model: Option<&str>) -> Result<Option<String>, Fail> {
    match model {
        None => Ok(None),
        Some(m) => {
            agy.require_supported()?;
            crate::models::resolve(agy, m).map(Some)
        }
    }
}

// ---------------------------------------------------------------------------
// check / help / version
// ---------------------------------------------------------------------------

pub(crate) fn check() -> CmdResult {
    let mut m = Map::new();
    match agy::find_agy() {
        None => {
            m.set("installed", false);
            m.set("path", "");
            m.set("version", "");
            m.set("auth", "unknown");
            m.set(
                "error",
                format!("agy binary not found; install with: {}", agy::INSTALL_HINT),
            );
        }
        Some(p) => {
            let a = Agy::at(p.clone());
            let version = a.version().unwrap_or("unknown").to_string();
            m.set("installed", true);
            m.set("path", p.display().to_string());
            m.set("version", version.clone());
            m.set("auth", agy::auth_status().as_str());
            m.set("pluginVersion", VERSION);
            let supported =
                version != "unknown" && agy::version_at_least(&version, agy::MIN_AGY_VERSION);
            m.set("supported", supported);
            m.set(
                "error",
                if supported || version == "unknown" {
                    String::new()
                } else {
                    format!("agy {version} is too old; this plugin needs {} or newer (run `agy update`)", agy::MIN_AGY_VERSION)
                },
            );
        }
    }
    println!("{}", json::to_string_compact(&Value::Object(m)));
    Ok(0)
}

pub(crate) fn version() -> CmdResult {
    let mut m = Map::new();
    m.set("version", VERSION);
    println!("{}", json::to_string_compact(&Value::Object(m)));
    Ok(0)
}

const HELP_HEAD: &str = "\
/agy:* commands (Claude Code plugin for the Antigravity CLI)

Read-only (agy can read, not change anything)
  /agy:ask [--model A] <prompt>         One-shot prompt; returns agy's answer.
  /agy:review [--base REF] [--background] [--wait] [--model A] [focus]
                                        Review the working-tree diff (or branch vs REF).
  /agy:adversarial-review [same flags] [focus]
                                        Challenge the design, not just the lines.
  /agy:scrape [--model A] <url>         Fetch a web page -> Markdown. SSRF-guarded.
  /agy:doc-to-md [--model A] <path>     Convert a local document -> Markdown (allow-listed types).

Write-capable (agy runs with auto-approve)
  /agy:rescue [--isolate] [--allow-dirty] [--background] [--wait] [--resume|--fresh] [--model A] <task>
                                        Let agy edit files. --isolate works in a throwaway
                                        git worktree and gives you a patch instead.
  /agy:image [--name S] [--output P] <description>
                                        Generate an image with agy's built-in tool.

Jobs (background review / rescue)
  /agy:status [id]    /agy:result [id]    /agy:cancel [id]

Other
  /agy:setup          Check that agy is installed, logged in and new enough.
  /agy:delegate, /agy:research   Hand work to the agy:runner subagent (uses ask).
  /agy:help           This help.

Model selection (--model)
";

const HELP_TAIL: &str = "
How --model works
  Aliases resolve at call time against `agy models` to the newest model in
  their family, so a new Gemini/Claude release is picked up without a plugin
  update. Any id or display name that `agy models` lists is also accepted
  verbatim. The result goes to agy's own --model flag for that call only;
  settings.json is never touched. Unknown names fail with exit 64.

Read-only commands
  agy runs from a throwaway directory (your repo is outside its workspace),
  pinned to its Default agent mode for that run, without
  --dangerously-skip-permissions: it can read, but writes outside that
  directory are denied and reported. agy 1.5.7+ would otherwise start
  headless runs in Auto mode, which approves such writes.

Requires agy 1.2.0 or newer. `agy --help` lists agy's own flags.
";

pub(crate) fn help() -> CmdResult {
    let models = agy::find_agy()
        .map(Agy::at)
        .as_ref()
        .and_then(crate::models::list_models);
    print!(
        "{HELP_HEAD}{}{HELP_TAIL}",
        crate::models::format_model_table(models.as_deref())
    );
    Ok(0)
}

// ---------------------------------------------------------------------------
// ask / scrape / doc-to-md
// ---------------------------------------------------------------------------

pub(crate) fn ask(args: &[String]) -> CmdResult {
    let (model, rest) = take_model(args)?;
    let Some(prompt) = rest.first().filter(|p| !p.is_empty()) else {
        return Err(Fail::usage("error: ask requires a prompt argument"));
    };
    // Everything after the prompt is forwarded to agy (agents/runner.md
    // documents this), but only flags that can't widen agy's access.
    let extra = rest[1..].to_vec();
    capture::check_passthrough(&extra)?;
    let agy = Agy::ready()?;
    let model = resolve_model(&agy, model.as_deref())?;
    let timeout = timeout_env("AGY_ASK_TIMEOUT", "8m0s");
    let outcome = capture::run_readonly(
        &agy,
        &Run {
            prompt,
            model: model.as_deref(),
            timeout: &timeout,
            extra: &extra,
            workspace: Workspace::Fresh { stage: None },
        },
    )?;
    capture::report(&outcome)
}

pub(crate) fn scrape(args: &[String]) -> CmdResult {
    let (model, rest) = take_model(args)?;
    let Some(url) = rest.first().filter(|u| !u.is_empty()) else {
        return Err(Fail::usage(
            "error: scrape requires a URL argument (http/https)",
        ));
    };
    let safe = crate::guard::validate_scrape_url(url)
        .map_err(|r| Fail::refused(format!("error: refusing to scrape this URL — {r}")))?;
    let agy = Agy::ready()?;
    agy.require_supported()?;
    agy.readonly_mode_args()?;
    let model = resolve_model(&agy, model.as_deref())?;
    // The plugin downloads the page itself (SSRF-checked, DNS-pinned,
    // redirects re-checked) and agy only converts the saved copy: a
    // read-only agy run has no way to fetch a URL.
    let download = TempDir::new("agy-fetch-")
        .map_err(|e| Fail::runtime(format!("error: could not create a temp dir: {e}")))?;
    let saved = download.path().join("page");
    let page = crate::fetch::fetch(&safe, &saved, &crate::fetch::guard_policy())
        .map_err(|r| Fail::refused(format!("error: refusing to scrape this URL — {r}")))?;
    let staged = format!("page.{}", page.ext);
    let prompt = format!(
        "Read the file named \"{staged}\" in your workspace. It is the web page downloaded from {} (use that address to resolve relative links). Return its MAIN readable content as clean Markdown — preserve headings, lists, links, tables, and code blocks; drop nav/ads/boilerplate. Output ONLY the Markdown, no preamble.",
        page.final_url
    );
    let timeout = timeout_env("AGY_SCRAPE_TIMEOUT", "5m0s");
    let outcome = capture::run_readonly(
        &agy,
        &Run {
            prompt: &prompt,
            model: model.as_deref(),
            timeout: &timeout,
            extra: &[],
            workspace: Workspace::Fresh {
                stage: Some((&saved, &staged)),
            },
        },
    )?;
    capture::report(&outcome)
}

pub(crate) fn doc_to_md(args: &[String]) -> CmdResult {
    let (model, rest) = take_model(args)?;
    let Some(input) = rest.first().filter(|p| !p.is_empty()) else {
        return Err(Fail::usage("error: doc-to-md requires a file path"));
    };
    let cwd = std::env::current_dir()
        .map_err(|e| Fail::runtime(format!("error: no current directory: {e}")))?;
    // Without a home dir every ~/... deny entry would be meaningless: fail
    // closed rather than check against nothing.
    let home = util::home_dir()
        .filter(|h| h.is_absolute())
        .ok_or_else(|| Fail::refused("error: refusing to convert this file — cannot determine your home directory to check it against the sensitive-path list"))?;
    let real = crate::guard::validate_doc_path(input, &cwd, &home)
        .map_err(|r| Fail::refused(format!("error: refusing to convert this file — {r}")))?;
    let agy = Agy::ready()?;
    let model = resolve_model(&agy, model.as_deref())?;
    // A FIXED name from the validated extension: the untrusted original file
    // name never reaches agy's prompt.
    let ext = real
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let staged = format!("document.{ext}");
    let prompt = format!(
        "Read the file named \"{staged}\" in your workspace and convert it to clean, well-structured Markdown. Preserve headings, lists, tables, links, and code blocks. Output ONLY the Markdown — no preamble, no commentary."
    );
    let timeout = timeout_env("AGY_DOCTOMD_TIMEOUT", "8m0s");
    let outcome = capture::run_readonly(
        &agy,
        &Run {
            prompt: &prompt,
            model: model.as_deref(),
            timeout: &timeout,
            extra: &[],
            workspace: Workspace::Fresh {
                stage: Some((&real, &staged)),
            },
        },
    )?;
    capture::report(&outcome)
}

// ---------------------------------------------------------------------------
// The job runner (used in-process for foreground jobs and by `_run-job`)
// ---------------------------------------------------------------------------

fn meta_str<'a>(rec: &'a Value, key: &str) -> Option<&'a str> {
    rec.get("meta")
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
}

fn record_args(rec: &Value) -> Vec<String> {
    rec.get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn record_model(rec: &Value) -> Option<String> {
    rec.get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Run the agy part of a job record and write its output to `sink`.
pub(crate) fn default_runner(rec: &Value, sink: &mut Sink) -> Option<i32> {
    let id = jobs::id_of(rec).to_string();
    let agy = Agy::at(PathBuf::from(meta_str(rec, "agyBin").unwrap_or("agy")));
    let prompt = meta_str(rec, "prompt").unwrap_or("").to_string();
    let model = record_model(rec);
    let args = record_args(rec);
    match rec.get("kind").and_then(Value::as_str) {
        Some("review") | Some("adversarial-review") => {
            let Some(stage) = meta_str(rec, "stageDir").map(PathBuf::from) else {
                sink.emit(&format!(
                    "[agy-job {id}] error: review job has no stage dir\n"
                ));
                return Some(1);
            };
            let timeout = timeout_env("AGY_REVIEW_TIMEOUT", "10m0s");
            let res = capture::run_readonly(
                &agy,
                &Run {
                    prompt: &prompt,
                    model: model.as_deref(),
                    timeout: &timeout,
                    extra: &args,
                    workspace: Workspace::Existing(&stage),
                },
            );
            // The worker is the stage dir's last reader.
            let _ = std::fs::remove_dir_all(&stage);
            match res {
                Err(f) => {
                    sink.emit(&format!("{}\n", f.message));
                    Some(f.code)
                }
                Ok(outcome) => {
                    for d in &outcome.denied {
                        sink.emit(&format!(
                            "[agy-job {id}] note: agy tried to use {} ({}), which this read-only command blocks.\n",
                            d.display_name, d.action
                        ));
                    }
                    if outcome.response.is_empty() {
                        sink.emit(&format!(
                            "[agy-job {id}] error: agy returned no answer (status: {}).\n",
                            outcome.status.as_deref().unwrap_or("?")
                        ));
                        return Some(1);
                    }
                    sink.emit(&format!("{}\n", outcome.response));
                    Some(
                        if outcome.status.as_deref().is_some_and(|s| s != "SUCCESS") {
                            1
                        } else {
                            0
                        },
                    )
                }
            }
        }
        Some("rescue") => run_rescue_agy(&agy, rec, &prompt, model.as_deref(), &args, sink),
        other => {
            sink.emit(&format!(
                "[agy-job {id}] error: don't know how to run a '{}' job\n",
                other.unwrap_or("?")
            ));
            Some(1)
        }
    }
}

/// Write-capable run: agy edits files in the execution root with
/// auto-approval. Safety comes from rescue's clean-tree / --isolate rails.
fn run_rescue_agy(
    agy: &Agy,
    rec: &Value,
    task: &str,
    model: Option<&str>,
    extra: &[String],
    sink: &mut Sink,
) -> Option<i32> {
    let root = meta_str(rec, "executionRoot")
        .or_else(|| rec.get("workspaceRoot").and_then(Value::as_str))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut args: Vec<String> = vec![
        "--dangerously-skip-permissions".into(),
        "--add-dir".into(),
        root.display().to_string(),
        "--output-format".into(),
        "json".into(),
    ];
    if let Some(m) = model {
        args.push("--model".into());
        args.push(m.into());
    }
    let timeout = timeout_env("AGY_RESCUE_TIMEOUT", "0");
    if timeout != "0" {
        args.push("--print-timeout".into());
        args.push(timeout.clone());
    }
    args.extend(extra.iter().cloned());
    args.push("--print".into());
    args.push(capture::cap_prompt(task));
    let mut cmd = agy.command();
    cmd.args(&args)
        .current_dir(&root)
        .env_remove("AGY_JOB_WORKSPACE");
    // --isolate: don't hand agy pointers to the real repository either.
    if meta_str(rec, "executionRoot").is_some() {
        for k in capture::REPO_ENV_HINTS {
            cmd.env_remove(k);
        }
    }
    let deadline = capture::parse_go_duration(&timeout)
        .filter(|d| !d.is_zero())
        .map(|d| d + Duration::from_secs(120));
    let out = match util::run_captured_notify(&mut cmd, deadline, &crate::jobs::note_child) {
        Ok(o) => o,
        Err(e) => {
            sink.emit(&format!("error: could not run agy: {e}\n"));
            return Some(127);
        }
    };
    let outcome = capture::parse_outcome(&out.stdout_str(), out.code, out.stderr_str());
    let err = out.stderr_str();
    if !err.trim().is_empty() {
        sink.emit(&format!("{}\n", err.trim_end()));
    }
    if !outcome.response.is_empty() {
        sink.emit(&format!("{}\n", outcome.response));
    }
    if out.timed_out {
        sink.emit("error: agy did not finish within AGY_RESCUE_TIMEOUT.\n");
        return None;
    }
    match (out.code, outcome.status.as_deref()) {
        (Some(0), Some("SUCCESS") | None) => Some(0),
        (Some(0), Some(_)) => Some(1),
        (code, _) => code,
    }
}

// ---------------------------------------------------------------------------
// review / adversarial-review
// ---------------------------------------------------------------------------

const REVIEW_SCHEMA: Schema<'static> = Schema {
    boolean: &["background", "wait"],
    value: &["model", "base"],
    passthrough_value: AGY_VALUE_FLAGS,
};

fn secret_block(hits: &[&str], what: &str) -> Result<(), Fail> {
    if hits.is_empty() {
        return Ok(());
    }
    let list: String = hits.iter().map(|h| format!("\n  - {h}")).collect();
    if util::env_nonempty("AGY_REVIEW_ALLOW_SECRETS").as_deref() != Some("1") {
        return Err(Fail::refused(format!(
            "error: {what} matches common secret patterns:{list}\n\nReviewing it would forward those values to Google's Gemini API.\nRemove them, or set AGY_REVIEW_ALLOW_SECRETS=1 to proceed."
        )));
    }
    eprintln!(
        "WARNING: {what} matches common secret patterns:{list}\nProceeding because AGY_REVIEW_ALLOW_SECRETS=1 — those values will be sent to Gemini."
    );
    Ok(())
}

pub(crate) fn review(args: &[String], adversarial: bool) -> CmdResult {
    let name = if adversarial {
        "adversarial-review"
    } else {
        "review"
    };
    let p = cli::parse_args(args, &REVIEW_SCHEMA);
    if !p.errors.is_empty() {
        return Err(usage_errors(&p.errors));
    }
    if p.value("model").is_some_and(|m| m.trim().is_empty()) {
        return Err(crate::models::empty_value_error(None));
    }
    capture::check_passthrough(&p.extra)?;
    let root = workspace_root();
    let agy = Agy::ready()?;
    agy.require_supported()?;
    // Fail closed BEFORE doing any work if agy can't be pinned read-only.
    agy.readonly_mode_args()?;
    let model = resolve_model(&agy, p.value("model"))?;

    let base = p.value("base");
    let ctx = match base {
        Some(b) => crate::git::branch_diff(&root, b)?,
        None => crate::git::working_tree_diff(&root)?,
    };
    if ctx.diff.trim().is_empty() {
        return Err(Fail::runtime(match (&ctx.scope, base) {
            (crate::prompts::Scope::Branch { merge_base, .. }, Some(b)) => format!(
                "No diff between this branch and `{b}` (merge-base: {}). Nothing to review.",
                merge_base.as_deref().unwrap_or("?")
            ),
            _ => "No working-tree diff. Stage or make changes first.".to_string(),
        }));
    }
    // Every line of the diff goes to Gemini — removed and context lines too
    // (`git rm .env` puts the whole secret file on `-` lines) — so scan all
    // of it, not only the added lines.
    secret_block(&crate::secrets::scan_text(&ctx.diff), "the diff")?;

    util::sweep_stale_temp(OUR_TEMP_PREFIXES, Duration::from_secs(24 * 3600));
    let stage = TempDir::new("agy-review-")
        .map_err(|e| Fail::runtime(format!("error: could not create the review stage dir: {e}")))?;
    let staged =
        crate::git::stage_review_materials(stage.path(), &ctx.root, &ctx.files, &ctx.diff)?;
    // Pass 2: the full content of every staged file is shipped as context,
    // including lines the diff never touched.
    let mut hits: Vec<&'static str> = Vec::new();
    for rel in &staged.staged {
        if let Ok(t) = std::fs::read_to_string(stage.path().join("files").join(rel)) {
            for h in crate::secrets::scan_text(&t) {
                if !hits.contains(&h) {
                    hits.push(h);
                }
            }
        }
    }
    secret_block(&hits, "the full content of a changed file")?;

    let focus = cli::join_positional(&p);
    let stage_str = stage.path().display().to_string();
    let prompt = if adversarial {
        crate::prompts::build_adversarial_prompt(
            &ctx.scope,
            &focus,
            &stage_str,
            &staged.staged,
            &staged.omitted,
        )
    } else {
        crate::prompts::build_review_prompt(
            &ctx.scope,
            &focus,
            &stage_str,
            &staged.staged,
            &staged.omitted,
        )
    };
    let mut task = name.to_string();
    if let Some(b) = base {
        task.push_str(&format!(" (--base {b})"));
    }
    if !focus.is_empty() {
        task.push_str(&format!(": {focus}"));
    }
    let background = p.flag("background");
    // The worker owns (and deletes) the stage dir from here on.
    let stage_path = stage.keep();
    let rec = jobs::create_job(
        &root,
        NewJob {
            kind: if adversarial {
                "adversarial-review"
            } else {
                "review"
            },
            task: task.clone(),
            model,
            args: p.extra.clone(),
            background,
            prompt,
            agy_bin: agy.path().display().to_string(),
            execution_root: None,
            stage_dir: Some(stage_path.clone()),
        },
    )
    .inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&stage_path);
    })?;
    let id = jobs::id_of(&rec).to_string();
    if !background {
        let st = jobs::run_worker(&root, &id, true, &default_runner)?;
        return Ok(if st == "completed" { 0 } else { 1 });
    }
    jobs::spawn_worker(&root, &id)?;
    if p.flag("wait") {
        println!("Started {task} job {id}. Waiting...");
        return wait_and_print(&root, &id, None);
    }
    println!(
        "Started {task} job {id} in background.\nCheck progress: /agy:status {id}\nRead output:    /agy:result {id}\nCancel:         /agy:cancel {id}"
    );
    Ok(0)
}

fn wait_and_print(root: &Path, id: &str, git_summary: Option<&Path>) -> CmdResult {
    let fin = jobs::wait_for_job(
        root,
        id,
        Duration::from_secs(600),
        Duration::from_millis(750),
    )?
    .ok_or_else(|| Fail::runtime(format!("Job {id} disappeared while waiting.")))?;
    let log = jobs::read_log(root, id)?;
    print!("{log}");
    if !log.is_empty() && !log.ends_with('\n') {
        println!();
    }
    if let Some(dir) = git_summary {
        println!(
            "\n=== changes agy made (review before committing) ===\n{}",
            crate::git::change_summary(dir)
        );
    }
    let st = jobs::status_of(&fin);
    println!("\nJob {id} ended with status: {st}");
    if st == "running" || st == "pending" {
        println!("(still running after 10 minutes — check later with /agy:status {id})");
    }
    Ok(if st == "completed" { 0 } else { 1 })
}

// ---------------------------------------------------------------------------
// rescue
// ---------------------------------------------------------------------------

const RESCUE_SCHEMA: Schema<'static> = Schema {
    boolean: &[
        "background",
        "wait",
        "resume",
        "fresh",
        "isolate",
        "allow-dirty",
    ],
    value: &["model"],
    passthrough_value: AGY_VALUE_FLAGS,
};

pub(crate) fn rescue(args: &[String]) -> CmdResult {
    let p = cli::parse_args(args, &RESCUE_SCHEMA);
    if !p.errors.is_empty() {
        return Err(usage_errors(&p.errors));
    }
    if p.flag("resume") && p.flag("fresh") {
        return Err(Fail::usage(
            "error: --resume and --fresh are mutually exclusive",
        ));
    }
    let task = cli::join_positional(&p);
    if task.is_empty() {
        return Err(Fail::usage("rescue: task description is required"));
    }
    if p.value("model").is_some_and(|m| m.trim().is_empty()) {
        return Err(crate::models::empty_value_error(None));
    }
    let root = workspace_root();
    let agy = Agy::ready()?;
    agy.require_supported()?;
    let model = resolve_model(&agy, p.value("model"))?;
    let mut extra = p.extra.clone();
    // --fresh is the default (a new conversation); --resume continues the
    // most recent one.
    if p.flag("resume") {
        extra.push("--continue".into());
    }
    let agy_bin = agy.path().display().to_string();

    if p.flag("isolate") {
        if !crate::git::is_repo(&root) {
            return Err(Fail::runtime(
                "rescue --isolate: requires a git repo (worktree isolation needs git).",
            ));
        }
        if p.flag("background") {
            println!(
                "[note] --isolate runs in the foreground so it can show you the patch; ignoring --background."
            );
        }
        let wt_parent = TempDir::new("agy-isolate-")
            .map_err(|e| Fail::runtime(format!("error: could not create a temp dir: {e}")))?;
        let wt = wt_parent.path().join("wt");
        crate::git::isolated_checkout(&root, &wt).map_err(|e| {
            Fail::runtime(format!(
                "rescue --isolate: could not create the isolated checkout: {e}"
            ))
        })?;
        let result = (|| -> CmdResult {
            let rec = jobs::create_job(
                &root,
                NewJob {
                    kind: "rescue",
                    task: task.clone(),
                    model: model.clone(),
                    args: extra.clone(),
                    background: false,
                    prompt: task.clone(),
                    agy_bin: agy_bin.clone(),
                    execution_root: Some(wt.clone()),
                    stage_dir: None,
                },
            )?;
            let id = jobs::id_of(&rec).to_string();
            let st = jobs::run_worker(&root, &id, true, &default_runner)?;
            let (stat, patch) = crate::git::isolated_patch(&wt);
            println!("\n=== isolated changes (your working tree was NOT touched) ===");
            if !stat.is_empty() && !patch.trim().is_empty() {
                println!("{stat}");
                let pf = jobs::patch_path(&root, &id);
                jobs::write_atomic(&pf, patch.as_bytes())
                    .map_err(|e| Fail::runtime(format!("error: could not save the patch: {e}")))?;
                let rel = pf
                    .strip_prefix(&root)
                    .map(Path::to_path_buf)
                    .unwrap_or(pf.clone());
                let rel = rel.display();
                println!(
                    "\nReview the patch:  {rel}\nApply it:          git apply \"{rel}\"\nDiscard it:        rm \"{rel}\"   (your tree is already untouched)\n"
                );
            } else {
                println!("(agy made no file changes.)");
            }
            Ok(if st == "completed" { 0 } else { 1 })
        })();
        crate::git::remove_isolated(&wt);
        return result;
    }

    let in_repo = crate::git::is_repo(&root);
    if in_repo {
        if !crate::git::is_clean(&root) && !p.flag("allow-dirty") {
            return Err(Fail::runtime(
                "rescue: refusing to run on a dirty working tree.\n        agy edits files in place with auto-approval, so a clean git\n        baseline is your only easy undo. Choose one:\n          - commit or stash your changes first, or\n          - run with --isolate (agy edits a throwaway worktree, you get a patch), or\n          - pass --allow-dirty to override (you accept the risk).",
            ));
        }
    } else {
        eprintln!(
            "[warn] not a git repo — no clean-tree safety net; agy will edit files in place with auto-approval."
        );
    }
    let background = p.flag("background");
    let rec = jobs::create_job(
        &root,
        NewJob {
            kind: "rescue",
            task: task.clone(),
            model,
            args: extra,
            background,
            prompt: task,
            agy_bin,
            execution_root: None,
            stage_dir: None,
        },
    )?;
    let id = jobs::id_of(&rec).to_string();
    if !background {
        let st = jobs::run_worker(&root, &id, true, &default_runner)?;
        if in_repo {
            println!(
                "\n=== changes agy made (review before committing) ===\n{}",
                crate::git::change_summary(&root)
            );
        }
        return Ok(if st == "completed" { 0 } else { 1 });
    }
    jobs::spawn_worker(&root, &id)?;
    if p.flag("wait") {
        println!("Started agy job {id} in background. Waiting...");
        return wait_and_print(&root, &id, in_repo.then_some(root.as_path()));
    }
    println!(
        "Started agy job {id} in background.\nCheck progress: /agy:status {id}\nRead output:    /agy:result {id}\nCancel:         /agy:cancel {id}\nWhen it finishes, run `git diff` to review what agy changed."
    );
    Ok(0)
}

// ---------------------------------------------------------------------------
// image (write-capable: agy needs auto-approval to run generate_image)
// ---------------------------------------------------------------------------

/// The image source must lie inside agy's own artifact dirs; a prompt-
/// injected marker could otherwise make us copy any host file to --output.
pub(crate) fn image_source_allowed(src: &Path, home: &Path) -> bool {
    let Ok(real) = std::fs::canonicalize(src) else {
        return false;
    };
    ["brain", "scratch", "cache"].iter().any(|d| {
        let base = home.join(".gemini").join("antigravity-cli").join(d);
        let canon = std::fs::canonicalize(&base).unwrap_or(base.clone());
        (real.starts_with(&base) && real != base) || (real.starts_with(&canon) && real != canon)
    })
}

pub(crate) fn image(args: &[String]) -> CmdResult {
    let mut name: Option<String> = None;
    let mut output: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--name" | "--output" => {
                let Some(v) = args.get(i + 1).filter(|v| !v.is_empty()) else {
                    return Err(Fail::usage(if a == "--name" {
                        "error: --name requires a value (e.g. --name coffee_cup)"
                    } else {
                        "error: --output requires a path (e.g. --output /tmp/out.png)"
                    }));
                };
                if a == "--name" {
                    name = Some(v.clone())
                } else {
                    output = Some(v.clone())
                }
                i += 2;
                continue;
            }
            "--" => {
                positional.extend(args[i + 1..].iter().cloned());
                break;
            }
            _ => {}
        }
        if let Some(v) = a.strip_prefix("--name=") {
            if v.is_empty() {
                return Err(Fail::usage("error: --name= requires a non-empty value"));
            }
            name = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--output=") {
            if v.is_empty() {
                return Err(Fail::usage("error: --output= requires a non-empty path"));
            }
            output = Some(v.to_string());
        } else {
            positional.push(a.clone());
        }
        i += 1;
    }
    let description = positional.join(" ");
    if description.trim().is_empty() {
        return Err(Fail::usage("error: image requires a description"));
    }
    let agy = Agy::ready()?;
    agy.require_supported()?;
    let work = TempDir::new("agy-image-")
        .map_err(|e| Fail::runtime(format!("error: could not create temp dir: {e}")))?;
    let marker = work.path().join("image-path.txt");
    let name_clause = name
        .as_deref()
        .map(|n| format!(" Save the image with the name \"{n}\"."))
        .unwrap_or_default();
    let prompt = format!(
        "Use your built-in generate_image tool to create this image: {description}.{name_clause}\n\nAfter the image is saved, use the write_file tool to write ONLY the absolute filesystem path of the saved image (a single line, no quotes, nothing else) to this exact path:\n{}\nDo not print anything to chat. The marker file is your only textual deliverable.",
        marker.display()
    );
    let timeout = timeout_env("AGY_IMAGE_TIMEOUT", "8m0s");
    let mut cmd = agy.command();
    cmd.args(["--dangerously-skip-permissions", "--sandbox", "--add-dir"])
        .arg(work.path())
        .args(["--print-timeout", &timeout, "--print", &prompt])
        .current_dir(work.path());
    for k in capture::REPO_ENV_HINTS {
        cmd.env_remove(k);
    }
    let deadline = capture::parse_go_duration(&timeout).map(|d| d + Duration::from_secs(120));
    let _ = util::run_captured(&mut cmd, deadline);
    let src = std::fs::read_to_string(&marker).ok().and_then(|t| {
        t.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(PathBuf::from)
    });
    let Some(src) = src.filter(|s| s.is_file()) else {
        return Err(Fail::runtime(
            "[agy-plugin] error: agy did not produce a locatable image.\n[agy-plugin]        generate_image may have failed; check ~/.gemini/antigravity-cli/log/ or run `agy` interactively.",
        ));
    };
    let home = util::home_dir().unwrap_or_default();
    if !image_source_allowed(&src, &home) {
        return Err(Fail::new(
            66,
            format!(
                "[agy-plugin] error: refusing to use an image path outside agy's artifacts directory.\n[agy-plugin]        path: {}\n[agy-plugin]        allowed: ~/.gemini/antigravity-cli/{{brain,scratch,cache}}/",
                src.display()
            ),
        ));
    }
    println!("[agy-plugin] generated: {}", src.display());
    if let Some(out) = output {
        std::fs::copy(&src, &out)
            .map_err(|e| Fail::runtime(format!("error: could not copy the image to {out}: {e}")))?;
        println!("[agy-plugin] copied to: {out}");
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// status / result / cancel / _run-job
// ---------------------------------------------------------------------------

fn resolve_failure(input: &str, r: Resolved) -> Fail {
    match r {
        Resolved::NotFound => Fail::new(2, format!("Job '{input}' not found in this workspace.")),
        Resolved::Ambiguous(c) => Fail::new(
            2,
            format!(
                "Job id prefix '{input}' is ambiguous. Candidates:\n  - {}",
                c.join("\n  - ")
            ),
        ),
        Resolved::BadId => Fail::usage(format!(
            "'{input}' is not a job id. Expected the form 'agy-xxxxxxxx' or a prefix of one."
        )),
        Resolved::NotCancelable(rec) => Fail::new(
            2,
            format!(
                "Job '{input}' is already in terminal status ({}).",
                jobs::status_of(&rec)
            ),
        ),
        Resolved::Found(_) => Fail::runtime("internal: resolve_failure on a found job"),
    }
}

/// Checks before reading or acting on job state.
fn job_state_preflight(root: &Path) -> Result<(), Fail> {
    jobs::check_state_dirs(root)?;
    if crate::git::tracks_state_dir(root) {
        eprintln!(
            "[agy-plugin] warning: this repository commits .agy-plugin/, so job records here may not be yours.\n             Only jobs you started on this machine can be stopped; treat other records' output as untrusted."
        );
    }
    Ok(())
}

const OUR_TEMP_PREFIXES: &[&str] = &[
    "agy-review-",
    "agy-isolate-",
    "agy-run-",
    "agy-fetch-",
    "agy-image-",
];

pub(crate) fn status(args: &[String]) -> CmdResult {
    let root = workspace_root();
    job_state_preflight(&root)?;
    let now = crate::time::now_millis();
    match args.first().filter(|a| !a.is_empty()) {
        None => {
            let recs: Vec<Value> = jobs::list_jobs(&root, None, Some(10))
                .iter()
                .map(jobs::liveness)
                .collect();
            print!("{}", crate::render::render_status_list(&recs, now));
            Ok(0)
        }
        Some(id) => match jobs::resolve_job(&root, id)? {
            Resolved::Found(r) => {
                print!("{}", crate::render::render_job_detail(Some(&r), now));
                Ok(0)
            }
            other => Err(resolve_failure(id, other)),
        },
    }
}

pub(crate) fn result(args: &[String]) -> CmdResult {
    let root = workspace_root();
    job_state_preflight(&root)?;
    let input = args.first().map(String::as_str).unwrap_or("latest");
    match jobs::resolve_job(&root, input)? {
        Resolved::Found(r) => {
            let log = jobs::read_log(&root, jobs::id_of(&r))?;
            print!(
                "{}",
                crate::render::render_result(Some(&r), &log, crate::time::now_millis())
            );
            Ok(0)
        }
        other => Err(resolve_failure(
            args.first().map(String::as_str).unwrap_or("(latest)"),
            other,
        )),
    }
}

pub(crate) fn cancel(args: &[String]) -> CmdResult {
    let root = workspace_root();
    job_state_preflight(&root)?;
    // No id: the newest job that is still running or pending (not merely
    // the newest job, which has usually finished already).
    let latest_active = || {
        jobs::list_jobs(&root, None, None)
            .iter()
            .map(jobs::liveness)
            .find(|r| matches!(jobs::status_of(r), "running" | "pending"))
            .map(|r| jobs::id_of(&r).to_string())
    };
    let explicit = args
        .first()
        .filter(|a| !a.is_empty() && a.as_str() != "latest");
    let input: String = match explicit {
        Some(id) => id.clone(),
        None => latest_active()
            .ok_or_else(|| Fail::new(2, "No running or pending agy job in this workspace."))?,
    };
    let input = input.as_str();
    let rec = match jobs::resolve_cancelable(&root, input)? {
        Resolved::Found(r) => r,
        other => return Err(resolve_failure(input, other)),
    };
    let id = jobs::id_of(&rec).to_string();
    let outcome = match jobs::cancel_job(&root, &id, Duration::from_secs(5))? {
        CancelResult::Canceled(reason) => crate::render::CancelOutcome::Canceled {
            reason: reason.to_string(),
        },
        CancelResult::NotFound => crate::render::CancelOutcome::NotFound,
        CancelResult::NotCancelable(status) => {
            crate::render::CancelOutcome::NotCancelable { status }
        }
    };
    print!("{}", crate::render::render_cancel_report(&id, &outcome));
    Ok(
        if matches!(outcome, crate::render::CancelOutcome::Canceled { .. }) {
            0
        } else {
            1
        },
    )
}

pub(crate) fn run_job(args: &[String]) -> CmdResult {
    let Some(id) = args.first() else {
        return Err(Fail::usage("agy-plugin _run-job: missing job id"));
    };
    let root = util::env_nonempty("AGY_JOB_WORKSPACE")
        .map(PathBuf::from)
        .unwrap_or_else(workspace_root);
    let st = jobs::run_worker(&root, id, false, &default_runner)?;
    Ok(if st == "completed" || st == "canceled" {
        0
    } else {
        1
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn take_model_forms() {
        assert_eq!(
            take_model(&s(&["--model", "pro", "hi"])).unwrap(),
            (Some("pro".into()), s(&["hi"]))
        );
        assert_eq!(
            take_model(&s(&["--model=flash", "hi", "--effort", "high"])).unwrap(),
            (Some("flash".into()), s(&["hi", "--effort", "high"]))
        );
        assert_eq!(
            take_model(&s(&["--", "--model", "x"])).unwrap(),
            (None, s(&["--model", "x"]))
        );
        assert_eq!(take_model(&s(&["hi"])).unwrap(), (None, s(&["hi"])));
        assert_eq!(take_model(&s(&["--model"])).unwrap_err().code, 64);
        assert_eq!(take_model(&s(&["--model="])).unwrap_err().code, 64);
    }

    #[cfg(unix)]
    #[test]
    fn image_allowlist() {
        let home = TempDir::new("agy-home-").unwrap();
        let brain = home.path().join(".gemini/antigravity-cli/brain/x");
        std::fs::create_dir_all(&brain).unwrap();
        let ok = brain.join("ok.png");
        std::fs::write(&ok, "png").unwrap();
        assert!(image_source_allowed(&ok, home.path()));
        assert!(!image_source_allowed(Path::new("/etc/passwd"), home.path()));
        let link = brain.join("evil.png");
        std::os::unix::fs::symlink("/etc/passwd", &link).unwrap();
        assert!(
            !image_source_allowed(&link, home.path()),
            "symlink out of brain/"
        );
        let dotdot = home
            .path()
            .join(".gemini/antigravity-cli/brain/../../../../../../etc/passwd");
        assert!(!image_source_allowed(&dotdot, home.path()));
        assert!(!image_source_allowed(Path::new(""), home.path()));
        // A sibling dir whose name merely starts with "brain".
        let evil = home.path().join(".gemini/antigravity-cli/brainEVIL");
        std::fs::create_dir_all(&evil).unwrap();
        std::fs::write(evil.join("x.png"), "x").unwrap();
        assert!(!image_source_allowed(&evil.join("x.png"), home.path()));
    }
}
