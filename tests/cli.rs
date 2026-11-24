//! End-to-end tests: the real agy-plugin binary against examples/stub_agy,
//! with an isolated HOME. These pin the security contracts of every command.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
#[cfg(unix)]
use std::time::{Duration, Instant};

fn plugin_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agy-plugin"))
}

/// target/<profile>/examples/stub_agy. Located from the plugin binary
/// (always in target/<profile>/), not from the test executable: cargo 1.100
/// moved test executables under target/<profile>/build/<pkg>/<hash>/.
fn stub_bin() -> PathBuf {
    let plugin = plugin_bin();
    let profile_dir = plugin.parent().unwrap();
    let name = if cfg!(windows) {
        "stub_agy.exe"
    } else {
        "stub_agy"
    };
    let p = profile_dir.join("examples").join(name);
    assert!(
        p.is_file(),
        "stub not built at {} (cargo test builds examples)",
        p.display()
    );
    p
}

struct Env {
    _dir: TempDir,
    root: PathBuf,
    home: PathBuf,
    record: PathBuf,
    prompt_out: PathBuf,
}

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> TempDir {
        let base = std::env::temp_dir().join(format!(
            "agy-it-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        {
            // Canonical (macOS /var -> /private/var), but without Windows'
            // verbatim \\?\ prefix, which the plugin never produces.
            let c = std::fs::canonicalize(&base).unwrap();
            let t = c.to_string_lossy();
            TempDir(PathBuf::from(
                t.strip_prefix(r"\\?\").unwrap_or(&t).to_string(),
            ))
        }
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Env {
    fn new(tag: &str) -> Env {
        let dir = TempDir::new(tag);
        let root = dir.0.clone();
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".gemini").join("antigravity-cli")).unwrap();
        Env {
            record: root.join("record.txt"),
            prompt_out: root.join("prompt.txt"),
            root,
            home,
            _dir: dir,
        }
    }

    fn cmd(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(plugin_bin());
        c.args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("AGY_PLUGIN_STATE_DIR", self.root.join("state"))
            .env("AGY_BIN", stub_bin())
            .env("STUB_RECORD", &self.record)
            .env("STUB_PROMPT_OUT", &self.prompt_out)
            .env("STUB_MODELS", "gemini-3.8-flash-low\\tGemini 3.8 Flash (Low)\\nclaude-opus-4-6-thinking\\tClaude Opus 4.6 (Thinking)\\n")
            .env("STUB_RESPONSE", "stub answer")
            .env_remove("ANTIGRAVITY_API_KEY")
            .env_remove("CLAUDE_PROJECT_DIR")
            .env_remove("AGY_REVIEW_ALLOW_SECRETS")
            .env("AGY_PLUGIN_TEST", "1");
        c
    }

    fn run(&self, cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut c = self.cmd(cwd, args);
        for (k, v) in envs {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    /// Recorded stub invocations: (cwd, argv, watched env).
    fn calls(&self) -> Vec<(String, Vec<String>, Vec<String>)> {
        std::fs::read_to_string(&self.record)
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let mut parts = l.splitn(3, '\t');
                let cwd = parts.next().unwrap_or("").to_string();
                let argv = parts
                    .next()
                    .unwrap_or("")
                    .split('\x1f')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                let env = parts
                    .next()
                    .unwrap_or("")
                    .split('\x1f')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                (cwd, argv, env)
            })
            .collect()
    }

    /// The agy runs that actually executed a prompt.
    fn print_calls(&self) -> Vec<(String, Vec<String>, Vec<String>)> {
        self.calls()
            .into_iter()
            .filter(|(_, a, _)| a.iter().any(|x| x == "--print"))
            .collect()
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn repo(env: &Env) -> PathBuf {
    let r = env.root.join("repo");
    std::fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q", "-b", "main"]);
    git(&r, &["config", "user.email", "t@t.t"]);
    git(&r, &["config", "user.name", "t"]);
    git(&r, &["config", "commit.gpgsign", "false"]);
    std::fs::write(
        r.join("calc.js"),
        "function add(a,b){return a+b}\nmodule.exports={add}\n",
    )
    .unwrap();
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-qm", "init"]);
    r
}

fn assert_readonly_contract(env: &Env, call: &(String, Vec<String>, Vec<String>), caller: &Path) {
    let (cwd, argv, envs) = call;
    let ws_idx = argv
        .iter()
        .position(|a| a == "--add-dir")
        .expect("--add-dir");
    let ws = &argv[ws_idx + 1];
    let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
    assert_eq!(
        canon(cwd),
        canon(ws),
        "agy runs in its workspace (cwd == --add-dir)"
    );
    assert_ne!(
        Path::new(cwd),
        caller,
        "agy must not run in the caller's dir"
    );
    assert!(argv.contains(&"--sandbox".to_string()));
    assert!(
        !argv
            .iter()
            .any(|a| a.contains("dangerously-skip-permissions")),
        "{argv:?}"
    );
    let p = argv.iter().position(|a| a == "--print").unwrap();
    assert!(
        argv[p - 1].starts_with("--settings={\"agentMode\":\"\"")
            && argv[p - 1].contains("command(*)")
            && argv[p - 1].contains("write_file(*)"),
        "the one-token mode pin must come last: {argv:?}"
    );
    assert!(
        argv.windows(2)
            .any(|w| w[0] == "--output-format" && w[1] == "json")
    );
    assert!(envs.is_empty(), "repo-location env leaked to agy: {envs:?}");
    let _ = env;
}

#[test]
fn ask_runs_readonly_in_a_throwaway_dir_with_the_pin_last() {
    let env = Env::new("ask");
    let caller = env.root.clone();
    let o = env.run(
        &caller,
        &["ask", "What is 6*7?"],
        &[
            ("CLAUDE_PROJECT_DIR", caller.to_str().unwrap()),
            ("GIT_DIR", "/x/.git"),
            ("AGY_JOB_WORKSPACE", "/x"),
        ],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stdout(&o), "stub answer\n");
    let calls = env.print_calls();
    assert_eq!(calls.len(), 1);
    assert_readonly_contract(&env, &calls[0], &caller);
    let prompt = std::fs::read_to_string(&env.prompt_out).unwrap();
    assert!(prompt.starts_with("What is 6*7?"), "{prompt}");
    assert!(
        prompt.contains("read-only session"),
        "agy is told which tools it has: {prompt}"
    );
    assert!(
        !Path::new(&calls[0].0).exists(),
        "throwaway workspace removed afterwards"
    );
}

#[test]
fn ask_model_alias_resolves_to_the_listed_id() {
    let env = Env::new("model");
    let o = env.run(&env.root, &["ask", "--model", "FLASH-LOW", "hi"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let argv = &env.print_calls()[0].1;
    assert!(
        argv.windows(2)
            .any(|w| w[0] == "--model" && w[1] == "gemini-3.8-flash-low"),
        "{argv:?}"
    );
    let o = env.run(&env.root, &["ask", "--model", "pro", "hi"], &[]);
    assert_eq!(o.status.code(), Some(64));
    assert!(stderr(&o).contains("no matching model"));
    let o = env.run(&env.root, &["ask", "--model"], &[]);
    assert_eq!(o.status.code(), Some(64));
}

#[test]
fn blocked_writes_are_reported_without_the_skip_permissions_hint() {
    let env = Env::new("denied");
    let o = env.run(
        &env.root,
        &["ask", "overwrite /etc/hosts"],
        &[
            ("STUB_RESPONSE", ""),
            ("STUB_DENIED", "write_file"),
            (
                "STUB_STDERR",
                "jetski: re-run with --dangerously-skip-permissions to auto-approve all tools.",
            ),
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    let e = stderr(&o);
    assert!(e.contains("read-only command blocks"), "{e}");
    assert!(!e.contains("dangerously"), "must not relay the hint: {e}");
}

#[test]
fn dangerous_passthrough_flags_are_refused_before_agy_runs() {
    let env = Env::new("passthrough");
    for extra in [
        &["--mode", "auto"][..],
        &["--dangerously-skip-permissions"],
        &["--settings={}"],
        &["--add-dir", "/"],
    ] {
        let mut args = vec!["ask", "hi"];
        args.extend_from_slice(extra);
        let o = env.run(&env.root, &args, &[]);
        assert_eq!(o.status.code(), Some(64), "{extra:?}");
    }
    assert!(env.print_calls().is_empty());
    let o = env.run(
        &env.root,
        &["ask", "hi", "--print-timeout", "1m", "--effort", "low"],
        &[],
    );
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn fails_closed_when_agy_defaults_to_auto_and_cannot_be_pinned() {
    let env = Env::new("nopin");
    let o = env.run(
        &env.root,
        &["ask", "hi"],
        &[
            ("STUB_VERSION", "1.5.9"),
            ("STUB_HELP", "Usage of agy:\n  --output-format x"),
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("Refusing to run a read-only command"));
    assert!(env.print_calls().is_empty(), "agy must not run");
}

#[test]
fn too_old_agy_and_missing_login_are_reported() {
    let env = Env::new("old");
    let o = env.run(&env.root, &["ask", "hi"], &[("STUB_VERSION", "1.1.5")]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("too old"));
    std::fs::remove_dir_all(env.home.join(".gemini")).unwrap();
    let o = env.run(&env.root, &["ask", "hi"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("not authenticated"));
}

#[test]
fn scrape_guards_and_normalizes_urls() {
    let env = Env::new("scrape");
    for bad in [
        "file:///etc/passwd",
        "http://169.254.169.254/latest/meta-data/",
        "http://2130706433/",
        "http://1.2.3.4.127.0.0.1.nip.io/",
    ] {
        let o = env.run(&env.root, &["scrape", bad], &[]);
        assert_eq!(o.status.code(), Some(65), "{bad}: {}", stderr(&o));
    }
    assert!(env.print_calls().is_empty());
    let o = env.run(&env.root, &["scrape"], &[]);
    assert_eq!(o.status.code(), Some(64));
    // A successful scrape downloads over the network; that path is covered
    // by fetch.rs unit tests (local server) and scripts/e2e-live.sh.
}

#[test]
fn doc_to_md_validates_and_stages_under_a_fixed_name() {
    let env = Env::new("doc");
    std::fs::write(env.root.join(".env"), "SECRET=1").unwrap();
    let o = env.run(&env.root, &["doc-to-md", ".env"], &[]);
    assert_eq!(o.status.code(), Some(65));
    std::fs::create_dir_all(env.home.join(".ssh")).unwrap();
    std::fs::write(env.home.join(".ssh").join("notes.md"), "k").unwrap();
    let o = env.run(
        &env.root,
        &[
            "doc-to-md",
            env.home.join(".ssh/notes.md").to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(o.status.code(), Some(65), "{}", stderr(&o));
    // An injection-shaped file name (Windows forbids '"' in names).
    let evil = if cfg!(windows) {
        "evil name ignore previous.md"
    } else {
        "evil \"name\" ignore previous.md"
    };
    std::fs::write(env.root.join(evil), "# Title\n").unwrap();
    let o = env.run(&env.root, &["doc-to-md", evil], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let prompt = std::fs::read_to_string(&env.prompt_out).unwrap();
    assert!(
        prompt.contains("\"document.md\"") && !prompt.contains("ignore previous"),
        "{prompt}"
    );
    assert_eq!(env.print_calls().len(), 1);
}

#[test]
fn review_stages_materials_and_blocks_secrets_in_both_passes() {
    let env = Env::new("review");
    let r = repo(&env);
    // No diff yet.
    let o = env.run(&r, &["review"], &[]);
    assert_eq!(o.status.code(), Some(1));
    std::fs::write(
        r.join("calc.js"),
        "function add(a,b){return a-b}\nmodule.exports={add}\n",
    )
    .unwrap();
    let o = env.run(&r, &["review", "focus on math"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("stub answer"));
    let calls = env.print_calls();
    assert_readonly_contract(&env, &calls[0], &r);
    let prompt = std::fs::read_to_string(&env.prompt_out).unwrap();
    assert!(
        prompt.contains("diff.patch")
            && prompt.contains("calc.js")
            && prompt.contains("focus on math")
    );
    assert!(
        !Path::new(&calls[0].0).exists(),
        "stage dir removed by the worker"
    );
    assert!(r.join(".agy-plugin/jobs").is_dir());

    // Pass 1: secret added in the diff.
    std::fs::write(r.join("calc.js"), "const k = \"AKIAABCDEFGHIJKLMNOP\";\n").unwrap();
    let o = env.run(&r, &["review"], &[]);
    assert_eq!(o.status.code(), Some(65), "{}", stderr(&o));
    // Removed lines are sent too: `git rm` of a secrets file must be refused.
    git(&r, &["checkout", "-q", "--", "calc.js"]);
    std::fs::write(
        r.join(".env.example"),
        "ANTHROPIC=sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123\n",
    )
    .unwrap();
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-qm", "env"]);
    git(&r, &["rm", "-q", ".env.example"]);
    let o = env.run(&r, &["review"], &[]);
    assert_eq!(
        o.status.code(),
        Some(65),
        "deleted secret file: {}",
        stderr(&o)
    );
    git(&r, &["reset", "-q", "--hard", "HEAD"]);

    // Pass 2: secret on an unchanged line far outside the diff's context
    // window — only the full-file copy shipped to agy contains it.
    let filler: String = (0..60).map(|i| format!("const f{i}=0;\n")).collect();
    std::fs::write(
        r.join("cfg.js"),
        format!("const KEY=\"AKIAABCDEFGHIJKLMNOP\";\n{filler}const a=1;\n"),
    )
    .unwrap();
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-qm", "cfg"]);
    std::fs::write(
        r.join("cfg.js"),
        format!("const KEY=\"AKIAABCDEFGHIJKLMNOP\";\n{filler}const a=2;\n"),
    )
    .unwrap();
    let before = env.print_calls().len();
    let o = env.run(&r, &["review"], &[]);
    assert_eq!(o.status.code(), Some(65), "{}", stderr(&o));
    assert!(stderr(&o).contains("full content of a changed file"));
    assert_eq!(env.print_calls().len(), before, "agy never ran on a secret");
    let o = env.run(&r, &["review"], &[("AGY_REVIEW_ALLOW_SECRETS", "1")]);
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn background_review_status_result_and_wait() {
    let env = Env::new("bg");
    let r = repo(&env);
    std::fs::write(r.join("calc.js"), "x\n").unwrap();
    let o = env.run(&r, &["adversarial-review", "--background", "--wait"], &[]);
    assert!(o.status.success(), "{}{}", stdout(&o), stderr(&o));
    let out = stdout(&o);
    assert!(
        out.contains("stub answer") && out.contains("ended with status: completed"),
        "{out}"
    );
    let o = env.run(&r, &["status"], &[]);
    assert!(stdout(&o).contains("[done]") && stdout(&o).contains("adversarial-review"));
    let o = env.run(&r, &["result"], &[]);
    assert!(stdout(&o).contains("--- output ---") && stdout(&o).contains("stub answer"));
    let o = env.run(&r, &["status", "agy-zz"], &[]);
    assert_eq!(o.status.code(), Some(64));
    let o = env.run(&r, &["status", "agy-00000000"], &[]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn rescue_rails_dirty_tree_and_isolate() {
    let env = Env::new("rescue");
    let r = repo(&env);
    let o = env.run(&r, &["rescue"], &[]);
    assert_eq!(o.status.code(), Some(64));
    std::fs::write(r.join("dirty.txt"), "x").unwrap();
    let o = env.run(&r, &["rescue", "fix", "it"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("dirty working tree"));
    assert!(env.print_calls().is_empty());
    std::fs::remove_file(r.join("dirty.txt")).unwrap();

    // Unknown agy flag before the task no longer swallows the task.
    let o = env.run(&r, &["rescue", "--sandbox", "fix", "the", "bug"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let (cwd, argv, _) = env.print_calls().pop().unwrap();
    assert_eq!(Path::new(&cwd), r.as_path());
    assert!(argv.contains(&"--dangerously-skip-permissions".to_string()));
    assert!(argv.contains(&"--sandbox".to_string()));
    assert_eq!(
        std::fs::read_to_string(&env.prompt_out).unwrap(),
        "fix the bug"
    );

    // --isolate: the agy run happens in a throwaway worktree.
    let o = env.run(&r, &["rescue", "--isolate", "touch", "nothing"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let (cwd, _, _) = env.print_calls().pop().unwrap();
    assert_ne!(Path::new(&cwd), r.as_path());
    assert!(stdout(&o).contains("your working tree was NOT touched"));
    assert!(!Path::new(&cwd).exists(), "worktree removed");
}

/// Running = exists and is not a zombie (Linux: /proc state; elsewhere
/// kill -0).
#[cfg(unix)]
fn running(pid: u32) -> bool {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        return !stat
            .rsplit_once(')')
            .is_some_and(|r| r.1.trim_start().starts_with('Z'));
    }
    if cfg!(target_os = "linux") {
        return false;
    }
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(unix)]
#[test]
fn cancel_stops_a_background_job_and_its_agy() {
    let env = Env::new("cancel");
    let r = repo(&env);
    let stub_pid_file = env.root.join("stub.pid");
    let o = env.run(
        &r,
        &["rescue", "--background", "slow", "task"],
        &[
            ("STUB_SLEEP_MS", "60000"),
            ("STUB_PID_OUT", stub_pid_file.to_str().unwrap()),
        ],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let out = stdout(&o);
    let id = out
        .split_whitespace()
        .find(|w| w.starts_with("agy-"))
        .unwrap()
        .to_string();
    // Wait for the worker to start agy.
    let deadline = Instant::now() + Duration::from_secs(10);
    while env.print_calls().is_empty() {
        assert!(Instant::now() < deadline, "agy never started");
        std::thread::sleep(Duration::from_millis(50));
    }
    let pid: u32 = std::fs::read_to_string(r.join(format!(".agy-plugin/runtime/{id}.pid")))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let o = env.run(&r, &["cancel", &id], &[]);
    assert!(o.status.success(), "{}{}", stdout(&o), stderr(&o));
    assert!(stdout(&o).contains("canceled (killed)"));
    let o = env.run(&r, &["status", &id], &[]);
    assert!(stdout(&o).contains("[cancel]"));
    // The worker AND the agy it started are gone (a zombie awaiting its
    // reaper counts as gone).
    let agy_pid: u32 = std::fs::read_to_string(env.root.join("stub.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while (running(pid) || running(agy_pid)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!running(pid), "worker still running");
    assert!(!running(agy_pid), "agy still running");
    let o = env.run(&r, &["cancel", &id], &[]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn image_copies_only_from_agy_artifact_dirs() {
    let env = Env::new("image");
    let brain = env.home.join(".gemini/antigravity-cli/brain/c1");
    std::fs::create_dir_all(&brain).unwrap();
    let img = brain.join("red.png");
    std::fs::write(&img, "PNG").unwrap();
    let out = env.root.join("out.png");
    let o = env.run(
        &env.root,
        &[
            "image",
            "--output",
            out.to_str().unwrap(),
            "a",
            "red",
            "square",
        ],
        &[
            ("STUB_MARKER_FROM_PROMPT", "1"),
            ("STUB_IMAGE", img.to_str().unwrap()),
        ],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "PNG");
    let secret = env.root.join("secret.png");
    std::fs::write(&secret, "SECRET").unwrap();
    let o = env.run(
        &env.root,
        &["image", "x"],
        &[
            ("STUB_MARKER_FROM_PROMPT", "1"),
            ("STUB_IMAGE", secret.to_str().unwrap()),
        ],
    );
    assert_eq!(o.status.code(), Some(66), "{}", stderr(&o));
    let o = env.run(&env.root, &["image"], &[]);
    assert_eq!(o.status.code(), Some(64));
}

#[test]
fn check_help_version_and_unknown() {
    let env = Env::new("misc");
    let o = env.run(&env.root, &["check"], &[]);
    let s = stdout(&o);
    assert!(
        s.contains("\"installed\":true")
            && s.contains("\"version\":\"1.5.16\"")
            && s.contains("\"supported\":true"),
        "{s}"
    );
    let o = env.run(&env.root, &["help"], &[]);
    assert!(o.status.success() && stdout(&o).contains("Gemini 3.8 Flash (Low)"));
    let o = env.run(&env.root, &["version"], &[]);
    assert!(stdout(&o).starts_with("{\"version\":\""));
    let o = env.run(&env.root, &["nope"], &[]);
    assert_eq!(o.status.code(), Some(64));
}
