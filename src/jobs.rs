//! Job state under `<workspace>/.agy-plugin/` and the worker that runs a job.
//!
//! The on-disk format is the one 0.8.x wrote (jobs/<id>.json,
//! logs/<id>.log, runtime/<id>.pid, patches/<id>.patch), so records from
//! older versions keep working with /agy:status, /agy:result, /agy:cancel.
//!
//! Fixes over 0.8.x:
//! - every read-modify-write of a record happens under a per-job lock file,
//!   and the WORKER (not the parent) marks a job running, so a fast worker's
//!   final status can no longer be overwritten with "running";
//! - background workers lead their own process group and cancel stops the
//!   whole group, so agy isn't orphaned;
//! - an agy killed by a signal is a failure, not exit code 0.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::json::{self, Map, Value};
use crate::util::{self, Fail};

pub(crate) const STATE_DIR: &str = ".agy-plugin";
pub(crate) const KINDS: &[&str] = &["rescue", "research", "review", "adversarial-review"];
pub(crate) const STATUSES: &[&str] = &["pending", "running", "completed", "failed", "canceled"];

pub(crate) fn state_dir(root: &Path) -> PathBuf {
    root.join(STATE_DIR)
}
pub(crate) fn record_path(root: &Path, id: &str) -> PathBuf {
    state_dir(root).join("jobs").join(format!("{id}.json"))
}
pub(crate) fn log_path(root: &Path, id: &str) -> PathBuf {
    state_dir(root).join("logs").join(format!("{id}.log"))
}
pub(crate) fn pid_path(root: &Path, id: &str) -> PathBuf {
    state_dir(root).join("runtime").join(format!("{id}.pid"))
}
fn lock_path(root: &Path, id: &str) -> PathBuf {
    state_dir(root).join("runtime").join(format!("{id}.lock"))
}
pub(crate) fn patch_path(root: &Path, id: &str) -> PathBuf {
    state_dir(root).join("patches").join(format!("{id}.patch"))
}

/// `agy-` + 8 lowercase hex digits, exactly what `generate_id` produces.
pub(crate) fn is_valid_id(s: &str) -> bool {
    s.len() == 12
        && s.starts_with("agy-")
        && s[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn generate_id() -> String {
    format!("agy-{}", util::random_hex(4))
}

fn io_fail(what: &str, e: std::io::Error) -> Fail {
    Fail::runtime(format!("error: {what}: {e}"))
}

/// Write via a private temp file + rename, so a reader never sees a torn
/// record and a crash leaves the old one intact. Mode 0600: records hold
/// the full prompt.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        util::random_hex(4)
    ));
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let res = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

fn write_record(root: &Path, id: &str, rec: &Value) -> Result<(), Fail> {
    let text = format!("{}\n", json::to_string_pretty(rec));
    write_atomic(&record_path(root, id), text.as_bytes())
        .map_err(|e| io_fail("could not write the job record", e))
}

/// Ok(None) for a missing or empty file; Err for unreadable or invalid JSON.
fn read_record_file(path: &Path) -> Result<Option<Value>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let v = json::parse(&text).map_err(|e| e.to_string())?;
    if v.as_object().is_none() {
        return Err("job record is not a JSON object".into());
    }
    Ok(Some(v))
}

/// Read a job; None for an unknown or malformed id (no disk access then).
pub(crate) fn read_job(root: &Path, id: &str) -> Result<Option<Value>, Fail> {
    if !is_valid_id(id) {
        return Ok(None);
    }
    read_record_file(&record_path(root, id))
        .map_err(|e| Fail::runtime(format!("error: job {id} is unreadable: {e}")))
}

/// Run `f` while holding `runtime/<id>.lock` (create-exclusive). A lock
/// older than 30 s is from a crashed process and is broken.
fn with_lock<T>(root: &Path, id: &str, f: impl FnOnce() -> Result<T, Fail>) -> Result<T, Fail> {
    let lp = lock_path(root, id);
    if let Some(d) = lp.parent() {
        std::fs::create_dir_all(d).map_err(|e| io_fail("could not create the job state dir", e))?;
    }
    let start = Instant::now();
    loop {
        match OpenOptions::new().write(true).create_new(true).open(&lp) {
            Ok(mut file) => {
                let _ = write!(file, "{}", std::process::id());
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::metadata(&lp)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_secs(30));
                if stale {
                    let _ = std::fs::remove_file(&lp);
                    continue;
                }
                if start.elapsed() > Duration::from_secs(10) {
                    return Err(Fail::runtime(format!(
                        "error: job {id} is locked by another process"
                    )));
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(e) => return Err(io_fail("could not lock the job record", e)),
        }
    }
    let res = f();
    let _ = std::fs::remove_file(&lp);
    res
}

/// Read-modify-write a job record under its lock. `f` edits the object;
/// `lastUpdatedAt` is set automatically and statuses are validated.
pub(crate) fn update_job(root: &Path, id: &str, f: impl FnOnce(&mut Map)) -> Result<Value, Fail> {
    with_lock(root, id, || {
        let mut rec = read_job(root, id)?
            .ok_or_else(|| Fail::runtime(format!("error: no such job '{id}'")))?;
        let obj = rec
            .as_object_mut()
            .ok_or_else(|| Fail::runtime("error: job record is not an object"))?;
        f(obj);
        if let Some(s) = obj.get("status").and_then(Value::as_str)
            && !STATUSES.contains(&s)
        {
            return Err(Fail::runtime(format!("error: invalid job status '{s}'")));
        }
        obj.set("id", id);
        obj.set("lastUpdatedAt", crate::time::iso_now());
        write_record(root, id, &rec)?;
        Ok(rec)
    })
}

pub(crate) struct NewJob {
    pub(crate) kind: &'static str,
    pub(crate) task: String,
    pub(crate) model: Option<String>,
    pub(crate) args: Vec<String>,
    pub(crate) background: bool,
    pub(crate) prompt: String,
    pub(crate) agy_bin: String,
    pub(crate) execution_root: Option<PathBuf>,
    pub(crate) stage_dir: Option<PathBuf>,
}

fn path_value(p: &Option<PathBuf>) -> Value {
    p.as_ref()
        .map(|p| Value::from(p.display().to_string()))
        .unwrap_or(Value::Null)
}

/// Create a pending job record (key order as 0.8.x wrote it).
pub(crate) fn create_job(root: &Path, job: NewJob) -> Result<Value, Fail> {
    if !KINDS.contains(&job.kind) {
        return Err(Fail::runtime(format!(
            "error: invalid job kind '{}'",
            job.kind
        )));
    }
    check_state_dirs(root)?;
    for sub in ["jobs", "logs", "runtime"] {
        std::fs::create_dir_all(state_dir(root).join(sub))
            .map_err(|e| io_fail("could not create the job state dir", e))?;
    }
    check_state_dirs(root)?;
    crate::git::ensure_exclude(root);
    let mut id = generate_id();
    while record_path(root, &id).exists() {
        id = generate_id();
    }
    let log_rel = Path::new(STATE_DIR).join("logs").join(format!("{id}.log"));
    let mut meta = Map::new();
    meta.set("prompt", job.prompt);
    meta.set("agyBin", job.agy_bin);
    meta.set("executionRoot", path_value(&job.execution_root));
    meta.set("stageDir", path_value(&job.stage_dir));
    let mut m = Map::new();
    m.set("id", id.clone());
    m.set("kind", job.kind);
    m.set("task", job.task);
    m.set("model", job.model);
    m.set("workspaceRoot", root.display().to_string());
    m.set("createdAt", crate::time::iso_now());
    m.set("startedAt", Value::Null);
    m.set("completedAt", Value::Null);
    m.set("status", "pending");
    m.set("exitCode", Value::Null);
    m.set("pid", Value::Null);
    m.set("logFile", log_rel.display().to_string());
    m.set(
        "args",
        Value::Array(job.args.into_iter().map(Value::from).collect()),
    );
    m.set("background", job.background);
    m.set("meta", meta);
    let rec = Value::Object(m);
    write_record(root, &id, &rec)?;
    crate::registry::register(&id, root, job.stage_dir.as_deref());
    Ok(rec)
}

/// Refuse a `.agy-plugin` (or a subdirectory) that is a symlink: a repo can
/// commit one, and records, logs and patches would then be read from and
/// written to wherever it points.
pub(crate) fn check_state_dirs(root: &Path) -> Result<(), Fail> {
    let base = state_dir(root);
    for p in [
        base.clone(),
        base.join("jobs"),
        base.join("logs"),
        base.join("runtime"),
        base.join("patches"),
    ] {
        if let Ok(m) = std::fs::symlink_metadata(&p)
            && m.file_type().is_symlink()
        {
            return Err(Fail::runtime(format!(
                "error: {} is a symlink; refusing to use it for job state.",
                p.display()
            )));
        }
    }
    Ok(())
}

fn str_field<'a>(rec: &'a Value, key: &str) -> Option<&'a str> {
    rec.get(key).and_then(Value::as_str)
}

pub(crate) fn id_of(rec: &Value) -> &str {
    str_field(rec, "id").unwrap_or("")
}

pub(crate) fn status_of(rec: &Value) -> &str {
    str_field(rec, "status").unwrap_or("")
}

fn pid_of(rec: &Value) -> Option<u32> {
    rec.get("pid")
        .and_then(Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
        .filter(|p| *p > 0)
}

/// All jobs, newest first; corrupt records are skipped with a note.
pub(crate) fn list_jobs(root: &Path, kind: Option<&str>, limit: Option<usize>) -> Vec<Value> {
    let Ok(entries) = std::fs::read_dir(state_dir(root).join("jobs")) else {
        return Vec::new();
    };
    let mut out: Vec<Value> = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".json") || name.starts_with('.') {
            continue;
        }
        match read_record_file(&e.path()) {
            Ok(Some(v)) => out.push(v),
            Ok(None) => {}
            Err(err) => eprintln!("agy-plugin: skipping corrupt job {name}: {err}"),
        }
    }
    out.sort_by(|a, b| {
        str_field(b, "createdAt")
            .unwrap_or("")
            .cmp(str_field(a, "createdAt").unwrap_or(""))
    });
    if let Some(k) = kind {
        out.retain(|r| str_field(r, "kind") == Some(k));
    }
    if let Some(n) = limit {
        out.truncate(n);
    }
    out
}

/// Is the job's worker running? Proven by its registry lock when this user
/// started the job here; otherwise (records from 0.8.x) a best-effort pid
/// check, which is only ever used for DISPLAY, never to send a signal.
fn worker_alive(rec: &Value) -> bool {
    let id = id_of(rec);
    if crate::registry::read(id).is_some() {
        // Our job: only a held lock counts (a bare pid may have been reused).
        return crate::registry::worker_alive(id) == Some(true);
    }
    pid_of(rec).is_some_and(crate::sys::process_alive)
}

/// A "running" record whose worker is gone is reported as failed (not
/// persisted; `wait_for_job` persists it).
pub(crate) fn liveness(rec: &Value) -> Value {
    let mut r = rec.clone();
    if status_of(rec) == "running"
        && !worker_alive(rec)
        && let Some(o) = r.as_object_mut()
    {
        o.set("status", "failed");
        o.set("livenessDowngrade", true);
    }
    r
}

pub(crate) enum Resolved {
    Found(Value),
    NotFound,
    Ambiguous(Vec<String>),
    BadId,
    NotCancelable(Value),
}

/// Resolve "", "latest", a full id or a unique prefix (`agy-3a7b`).
pub(crate) fn resolve_job(root: &Path, input: &str) -> Result<Resolved, Fail> {
    let raw = input.trim();
    if raw.is_empty() || raw == "latest" {
        return Ok(list_jobs(root, None, Some(1))
            .first()
            .map_or(Resolved::NotFound, |r| Resolved::Found(liveness(r))));
    }
    if is_valid_id(raw) {
        return Ok(
            read_job(root, raw)?.map_or(Resolved::NotFound, |r| Resolved::Found(liveness(&r)))
        );
    }
    let hex = raw.strip_prefix("agy-").unwrap_or("");
    if hex.len() < 2
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Ok(Resolved::BadId);
    }
    let matches: Vec<Value> = list_jobs(root, None, None)
        .into_iter()
        .filter(|r| id_of(r).starts_with(raw))
        .collect();
    Ok(match matches.len() {
        0 => Resolved::NotFound,
        1 => Resolved::Found(liveness(&matches[0])),
        _ => Resolved::Ambiguous(matches.iter().map(|r| id_of(r).to_string()).collect()),
    })
}

pub(crate) fn resolve_cancelable(root: &Path, input: &str) -> Result<Resolved, Fail> {
    Ok(match resolve_job(root, input)? {
        Resolved::Found(r) if !matches!(status_of(&r), "running" | "pending") => {
            Resolved::NotCancelable(r)
        }
        other => other,
    })
}

// ---------------------------------------------------------------------------
// Running a job
// ---------------------------------------------------------------------------

/// Where the worker's output goes: stdout (which IS the log for background
/// workers) and, in the foreground, also appended to the log file.
pub(crate) struct Sink {
    tee: Option<File>,
}

impl Sink {
    pub(crate) fn emit(&mut self, text: &str) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
        if let Some(f) = self.tee.as_mut() {
            let _ = f.write_all(text.as_bytes());
        }
    }
}

fn open_log_append(path: &Path) -> std::io::Result<File> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut o = OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// What a job's agy run produced: Some(0) success, Some(n) failure, None
/// killed/unknown.
pub(crate) type RunnerResult = Option<i32>;

/// Runs the agy part of a job. Injectable for tests.
pub(crate) type Runner<'a> = &'a dyn Fn(&Value, &mut Sink) -> RunnerResult;

/// Run job `id` in this process. `tee` mirrors output into the job log
/// (foreground); background workers already write stdout to the log.
/// Returns the final status.
pub(crate) fn run_worker(root: &Path, id: &str, tee: bool, runner: Runner) -> Result<String, Fail> {
    let mut sink = Sink {
        tee: if tee {
            open_log_append(&log_path(root, id)).ok()
        } else {
            None
        },
    };
    let me = std::process::id();
    let mut canceled_early = false;
    let rec = update_job(root, id, |o| {
        if o.get("status").and_then(Value::as_str) == Some("canceled") {
            canceled_early = true;
            return;
        }
        o.set("status", "running");
        o.set("pid", me);
        if o.get("startedAt").is_none_or(Value::is_null) {
            o.set("startedAt", crate::time::iso_now());
        }
    })?;
    if canceled_early {
        sink.emit(&format!(
            "[agy-job {id}] (job was canceled before it started)\n"
        ));
        return Ok("canceled".into());
    }
    let _ = write_atomic(&pid_path(root, id), format!("{me}\n").as_bytes());
    // Proof of life for /agy:cancel: held until this function returns (the
    // OS drops it if the process dies). Background workers (no tee) lead
    // their own process group.
    let _alive = crate::registry::hold(id);
    crate::registry::update(id, |e| {
        e.pid = Some(me);
        e.group_leader = !tee;
    });
    set_current_job(Some(id));
    sink.emit(&format!(
        "[agy-job {id}] starting at {}\n",
        crate::time::iso_now()
    ));
    sink.emit(&format!(
        "[agy-job {id}] task: {}\n",
        str_field(&rec, "task").unwrap_or("")
    ));
    if let Some(m) = str_field(&rec, "model").filter(|m| !m.is_empty()) {
        sink.emit(&format!("[agy-job {id}] model: {m}\n"));
    }
    sink.emit(&format!("[agy-job {id}] ---\n"));

    let exit = runner(&rec, &mut sink);
    set_current_job(None);
    let final_status = if exit == Some(0) {
        "completed"
    } else {
        "failed"
    };

    let mut was_canceled = false;
    update_job(root, id, |o| {
        if o.get("status").and_then(Value::as_str) == Some("canceled") {
            was_canceled = true;
            return;
        }
        o.set("status", final_status);
        o.set("exitCode", exit.map(i64::from));
        o.set("completedAt", crate::time::iso_now());
    })?;
    let _ = std::fs::remove_file(pid_path(root, id));
    drop(_alive);
    crate::registry::remove(id);
    if was_canceled {
        sink.emit(&format!(
            "[agy-job {id}] (job was canceled; leaving status=canceled)\n"
        ));
        return Ok("canceled".into());
    }
    sink.emit(&format!("[agy-job {id}] ---\n"));
    let code = exit.map_or_else(|| "null".to_string(), |c| c.to_string());
    sink.emit(&format!(
        "[agy-job {id}] done: status={final_status} exitCode={code}\n"
    ));
    Ok(final_status.into())
}

static CURRENT_JOB: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn set_current_job(id: Option<&str>) {
    if let Ok(mut g) = CURRENT_JOB.lock() {
        *g = id.map(str::to_string);
    }
}

/// Record the agy process the running job just started, so /agy:cancel can
/// stop it even when the worker isn't a process-group leader (foreground).
pub(crate) fn note_child(pid: u32) {
    let id = CURRENT_JOB.lock().ok().and_then(|g| g.clone());
    if let Some(id) = id {
        crate::registry::update(&id, |e| e.child_pid = Some(pid));
    }
}

/// Start the job in a detached worker process (`agy-plugin _run-job <id>`)
/// whose stdout/stderr append to the job log.
pub(crate) fn spawn_worker(root: &Path, id: &str) -> Result<u32, Fail> {
    let log = open_log_append(&log_path(root, id))
        .map_err(|e| io_fail("could not open the job log", e))?;
    let log2 = log
        .try_clone()
        .map_err(|e| io_fail("could not open the job log", e))?;
    let exe = std::env::current_exe().map_err(|e| io_fail("could not locate agy-plugin", e))?;
    let mut cmd = Command::new(exe);
    cmd.arg("_run-job")
        .arg(id)
        .current_dir(root)
        .env("AGY_JOB_WORKSPACE", root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2));
    crate::sys::detach(&mut cmd);
    let child = cmd
        .spawn()
        .map_err(|e| io_fail("could not start the background worker", e))?;
    let pid = child.id();
    let _ = write_atomic(&pid_path(root, id), format!("{pid}\n").as_bytes());
    crate::registry::update(id, |e| {
        e.pid = Some(pid);
        e.group_leader = true;
    });
    // The worker records its own pid when it starts; record it here too so a
    // cancel issued before that still finds the process.
    let _ = update_job(root, id, |o| {
        if o.get("pid").is_none_or(Value::is_null) {
            o.set("pid", pid);
        }
    });
    Ok(pid)
}

pub(crate) enum CancelResult {
    Canceled(&'static str),
    NotFound,
    NotCancelable(Option<String>),
}

/// Cancel a pending or running job: stop its worker's process tree, then
/// mark it canceled. Idempotent for finished jobs.
pub(crate) fn cancel_job(root: &Path, id: &str, grace: Duration) -> Result<CancelResult, Fail> {
    // Check and mark in one locked step, so a job finishing right now can't
    // be relabelled "canceled" after it completed.
    let marked = with_lock(root, id, || {
        let Some(mut rec) = read_job(root, id)? else {
            return Ok(Err(CancelResult::NotFound));
        };
        let st = status_of(&rec).to_string();
        if st != "running" && st != "pending" {
            return Ok(Err(CancelResult::NotCancelable(Some(st))));
        }
        if let Some(o) = rec.as_object_mut() {
            o.set("status", "canceled");
            o.set("completedAt", crate::time::iso_now());
            o.set("exitCode", Value::Null);
            o.set("lastUpdatedAt", crate::time::iso_now());
        }
        write_record(root, id, &rec)?;
        Ok(Ok(()))
    })?;
    if let Err(r) = marked {
        return Ok(r);
    }
    let _ = std::fs::remove_file(pid_path(root, id));
    // Signals and deletions use ONLY this user's registry, never the
    // workspace record (a repo can commit a record naming any pid or dir).
    let Some(entry) = crate::registry::read_for(id, root) else {
        return Ok(CancelResult::Canceled("not-started-here"));
    };
    let me = std::process::id();
    let reason = match crate::registry::worker_alive(id) {
        Some(true) => {
            if let Some(child) = entry.child_pid.filter(|p| *p != me) {
                crate::sys::terminate_tree(child, grace);
            }
            if let Some(pid) = entry.pid.filter(|p| *p != me) {
                crate::sys::terminate_tree(pid, grace);
            }
            "killed"
        }
        _ => "already-dead",
    };
    // A killed worker never reaches its own cleanup.
    if let Some(stage) = entry
        .stage_dir
        .as_deref()
        .filter(|p| crate::registry::is_our_stage_dir(p))
    {
        let _ = std::fs::remove_dir_all(stage);
    }
    crate::registry::remove(id);
    Ok(CancelResult::Canceled(reason))
}

/// Block until the job finishes or `timeout` passes. A "running" record
/// whose worker died is persisted as failed.
pub(crate) fn wait_for_job(
    root: &Path,
    id: &str,
    timeout: Duration,
    poll: Duration,
) -> Result<Option<Value>, Fail> {
    let deadline = Instant::now() + timeout;
    loop {
        let Some(rec) = read_job(root, id)? else {
            return Ok(None);
        };
        let st = status_of(&rec);
        if st != "running" && st != "pending" {
            return Ok(Some(rec));
        }
        if st == "running" && !worker_alive(&rec) {
            std::thread::sleep(poll);
            let Some(again) = read_job(root, id)? else {
                return Ok(None);
            };
            if matches!(status_of(&again), "running" | "pending") {
                return update_job(root, id, |o| {
                    o.set("status", "failed");
                    o.set("completedAt", crate::time::iso_now());
                    if let Some(meta) = o.get_mut("meta").and_then(Value::as_object_mut) {
                        meta.set("livenessDowngradedBy", "waitForJob");
                    }
                })
                .map(Some);
            }
            return Ok(Some(again));
        }
        if Instant::now() >= deadline {
            return Ok(Some(rec));
        }
        std::thread::sleep(poll);
    }
}

pub(crate) fn read_log(root: &Path, id: &str) -> Result<String, Fail> {
    match std::fs::read(log_path(root, id)) {
        Ok(b) => Ok(String::from_utf8_lossy(&b).into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(io_fail("could not read the job log", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::TempDir;

    fn new_job(kind: &'static str, background: bool) -> NewJob {
        NewJob {
            kind,
            task: "do the thing".into(),
            model: None,
            args: vec![],
            background,
            prompt: "prompt — with \"quotes\"".into(),
            agy_bin: "agy".into(),
            execution_root: None,
            stage_dir: None,
        }
    }

    #[test]
    fn ids() {
        let id = generate_id();
        assert!(is_valid_id(&id), "{id}");
        for bad in [
            "agy-DEADBEEF",
            "agy-deadbee",
            "agy-deadbeeff",
            "../../etc/passwd",
            "agy-deadbeef\n",
            "",
            "agy-../../x",
        ] {
            assert!(!is_valid_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn create_read_update_round_trip_in_the_0_8_format() {
        let d = TempDir::new("agy-jobs-test-").unwrap();
        let rec = create_job(d.path(), new_job("review", false)).unwrap();
        let id = id_of(&rec).to_string();
        let text = std::fs::read_to_string(record_path(d.path(), &id)).unwrap();
        let keys: Vec<&str> = rec.as_object().unwrap().iter().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            [
                "id",
                "kind",
                "task",
                "model",
                "workspaceRoot",
                "createdAt",
                "startedAt",
                "completedAt",
                "status",
                "exitCode",
                "pid",
                "logFile",
                "args",
                "background",
                "meta"
            ]
        );
        assert!(text.ends_with("}\n") && text.contains("\n  \"meta\": {\n    \"prompt\""));
        assert_eq!(status_of(&rec), "pending");
        let up = update_job(d.path(), &id, |o| o.set("status", "running")).unwrap();
        assert_eq!(status_of(&up), "running");
        assert!(up.get("lastUpdatedAt").is_some());
        assert!(update_job(d.path(), &id, |o| o.set("status", "bogus")).is_err());
        assert!(read_job(d.path(), "agy-00000000").unwrap().is_none());
        assert!(read_job(d.path(), "../../etc/passwd").unwrap().is_none());
        assert!(create_job(d.path(), new_job("nope", false)).is_err());
    }

    #[test]
    fn reads_a_record_written_by_0_8_x() {
        let d = TempDir::new("agy-jobs-test-").unwrap();
        std::fs::create_dir_all(d.path().join(".agy-plugin/jobs")).unwrap();
        std::fs::write(
            record_path(d.path(), "agy-2b067725"),
            "{\n  \"id\": \"agy-2b067725\",\n  \"kind\": \"review\",\n  \"task\": \"t\",\n  \"model\": null,\n  \"status\": \"completed\",\n  \"exitCode\": 0,\n  \"pid\": null,\n  \"createdAt\": \"2026-05-31T15:02:38.473Z\",\n  \"meta\": {}\n}\n",
        )
        .unwrap();
        match resolve_job(d.path(), "agy-2b").unwrap() {
            Resolved::Found(r) => assert_eq!(status_of(&r), "completed"),
            _ => panic!("not found"),
        }
    }

    #[test]
    fn list_sorts_filters_limits_and_skips_corrupt() {
        let d = TempDir::new("agy-jobs-test-").unwrap();
        let a = create_job(d.path(), new_job("review", false)).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let b = create_job(d.path(), new_job("rescue", false)).unwrap();
        std::fs::write(d.path().join(".agy-plugin/jobs/agy-broken0.json"), "{oops").unwrap();
        let all = list_jobs(d.path(), None, None);
        assert_eq!(
            all.iter().map(id_of).collect::<Vec<_>>(),
            vec![id_of(&b), id_of(&a)]
        );
        assert_eq!(list_jobs(d.path(), Some("review"), None).len(), 1);
        assert_eq!(list_jobs(d.path(), None, Some(1)).len(), 1);
        assert!(list_jobs(&d.path().join("nothing"), None, None).is_empty());
    }

    #[test]
    fn resolution_rules() {
        let d = TempDir::new("agy-jobs-test-").unwrap();
        assert!(matches!(
            resolve_job(d.path(), "").unwrap(),
            Resolved::NotFound
        ));
        let r = create_job(d.path(), new_job("review", false)).unwrap();
        let id = id_of(&r).to_string();
        assert!(matches!(
            resolve_job(d.path(), "latest").unwrap(),
            Resolved::Found(_)
        ));
        assert!(matches!(
            resolve_job(d.path(), &id).unwrap(),
            Resolved::Found(_)
        ));
        assert!(matches!(
            resolve_job(d.path(), &id[..6]).unwrap(),
            Resolved::Found(_)
        ));
        for bad in ["agy-", "agy-a", "AGY-1234", "../x", "agy-ZZ"] {
            assert!(
                matches!(resolve_job(d.path(), bad).unwrap(), Resolved::BadId),
                "{bad}"
            );
        }
        // Two jobs sharing a prefix -> ambiguous.
        std::fs::write(
            record_path(d.path(), "agy-aa000001"),
            "{\"id\":\"agy-aa000001\",\"createdAt\":\"1\"}",
        )
        .unwrap();
        std::fs::write(
            record_path(d.path(), "agy-aa000002"),
            "{\"id\":\"agy-aa000002\",\"createdAt\":\"2\"}",
        )
        .unwrap();
        match resolve_job(d.path(), "agy-aa").unwrap() {
            Resolved::Ambiguous(c) => assert_eq!(c.len(), 2),
            _ => panic!("expected ambiguous"),
        }
        assert!(
            matches!(
                resolve_cancelable(d.path(), &id).unwrap(),
                Resolved::Found(_)
            ),
            "pending is cancelable"
        );
    }

    #[test]
    fn worker_success_failure_and_signal() {
        let d = TempDir::new("agy-jobs-test-").unwrap();
        for (code, want) in [
            (Some(0), "completed"),
            (Some(2), "failed"),
            (None, "failed"),
        ] {
            let r = create_job(d.path(), new_job("review", false)).unwrap();
            let id = id_of(&r).to_string();
            let st = run_worker(d.path(), &id, true, &|_, s| {
                s.emit("answer\n");
                code
            })
            .unwrap();
            assert_eq!(st, want);
            let rec = read_job(d.path(), &id).unwrap().unwrap();
            assert_eq!(status_of(&rec), want);
            assert_eq!(
                rec.get("pid").and_then(Value::as_u64),
                Some(u64::from(std::process::id()))
            );
            let log = read_log(d.path(), &id).unwrap();
            assert!(
                log.contains("answer\n") && log.contains(&format!("done: status={want}")),
                "{log}"
            );
            assert!(!pid_path(d.path(), &id).exists());
        }
    }

    #[test]
    fn cancel_during_run_keeps_canceled_and_canceled_before_start_never_runs() {
        let d = TempDir::new("agy-jobs-test-").unwrap();
        let r = create_job(d.path(), new_job("review", false)).unwrap();
        let id = id_of(&r).to_string();
        let root = d.path().to_path_buf();
        let st = run_worker(d.path(), &id, false, &|rec, _| {
            // Simulate /agy:cancel arriving mid-run from another process.
            let jid = id_of(rec).to_string();
            update_job(&root, &jid, |o| o.set("status", "canceled")).unwrap();
            Some(0)
        })
        .unwrap();
        assert_eq!(st, "canceled");
        assert_eq!(
            status_of(&read_job(d.path(), &id).unwrap().unwrap()),
            "canceled"
        );

        let r2 = create_job(d.path(), new_job("review", false)).unwrap();
        let id2 = id_of(&r2).to_string();
        assert!(matches!(
            cancel_job(d.path(), &id2, Duration::from_millis(10)).unwrap(),
            CancelResult::Canceled(_)
        ));
        let ran = std::cell::Cell::new(false);
        assert_eq!(
            run_worker(d.path(), &id2, false, &|_, _| {
                ran.set(true);
                Some(0)
            })
            .unwrap(),
            "canceled"
        );
        assert!(!ran.get(), "a job canceled before start must not run");
        assert!(matches!(
            cancel_job(d.path(), &id2, Duration::from_millis(10)).unwrap(),
            CancelResult::NotCancelable(_)
        ));
        assert!(matches!(
            cancel_job(d.path(), "agy-00000000", Duration::from_millis(10)).unwrap(),
            CancelResult::NotFound
        ));
    }

    #[cfg(unix)]
    fn spawn_detached_sleep() -> (
        u32,
        std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>,
    ) {
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        crate::sys::detach(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let pid = child.id();
        (pid, std::thread::spawn(move || child.wait()))
    }

    #[cfg(unix)]
    #[test]
    fn cancel_stops_a_worker_proven_alive_by_its_registry_lock() {
        let d = TempDir::new("agy-jobs-test-").unwrap();
        let r = create_job(d.path(), new_job("rescue", true)).unwrap();
        let id = id_of(&r).to_string();
        let (pid, reaper) = spawn_detached_sleep();
        update_job(d.path(), &id, |o| {
            o.set("status", "running");
            o.set("pid", pid);
        })
        .unwrap();
        crate::registry::update(&id, |e| {
            e.pid = Some(pid);
            e.group_leader = true;
        });
        // The worker's proof of life (held by the worker process in real runs).
        let lock = crate::registry::hold(&id).unwrap();
        assert!(matches!(
            cancel_job(d.path(), &id, Duration::from_secs(3)).unwrap(),
            CancelResult::Canceled("killed")
        ));
        drop(lock);
        let _ = reaper.join();
        assert!(!crate::sys::process_alive(pid));
        assert_eq!(
            status_of(&read_job(d.path(), &id).unwrap().unwrap()),
            "canceled"
        );
        assert!(
            crate::registry::read(&id).is_none(),
            "registry entry cleaned up"
        );
    }

    /// A repository can commit `.agy-plugin/jobs/*.json`. A planted record
    /// naming someone's pid and directory must not make cancel kill or
    /// delete anything (independent review finding).
    #[cfg(unix)]
    #[test]
    fn a_planted_record_cannot_make_cancel_kill_or_delete() {
        let d = TempDir::new("agy-jobs-test-").unwrap();
        let victim = TempDir::new("agy-victim-").unwrap();
        std::fs::write(victim.path().join("precious.txt"), "keep me").unwrap();
        let (pid, reaper) = spawn_detached_sleep();
        std::fs::create_dir_all(d.path().join(".agy-plugin/jobs")).unwrap();
        std::fs::write(
            record_path(d.path(), "agy-deadbeef"),
            format!(
                "{{\"id\":\"agy-deadbeef\",\"kind\":\"rescue\",\"status\":\"running\",\"pid\":{pid},\"createdAt\":\"9999\",\"meta\":{{\"stageDir\":\"{}\"}}}}",
                victim.path().display()
            ),
        )
        .unwrap();
        assert!(matches!(
            cancel_job(d.path(), "agy-deadbeef", Duration::from_millis(200)).unwrap(),
            CancelResult::Canceled("not-started-here")
        ));
        assert!(
            crate::sys::process_alive(pid),
            "planted pid must not be signalled"
        );
        assert_eq!(
            std::fs::read_to_string(victim.path().join("precious.txt")).unwrap(),
            "keep me"
        );
        crate::sys::terminate_tree(pid, Duration::from_secs(2));
        let _ = reaper.join();
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_registry_entry_never_signals_a_reused_pid() {
        let d = TempDir::new("agy-jobs-test-").unwrap();
        let r = create_job(d.path(), new_job("rescue", true)).unwrap();
        let id = id_of(&r).to_string();
        // An unrelated live process now owns the pid the dead worker had.
        let (pid, reaper) = spawn_detached_sleep();
        update_job(d.path(), &id, |o| {
            o.set("status", "running");
            o.set("pid", pid);
        })
        .unwrap();
        crate::registry::update(&id, |e| {
            e.pid = Some(pid);
            e.group_leader = true;
        });
        // Nobody holds the lock: the worker is dead.
        let l = liveness(&read_job(d.path(), &id).unwrap().unwrap());
        assert_eq!(status_of(&l), "failed", "a dead worker is reported failed");
        assert!(matches!(
            cancel_job(d.path(), &id, Duration::from_millis(200)).unwrap(),
            CancelResult::Canceled("already-dead")
        ));
        assert!(
            crate::sys::process_alive(pid),
            "the reused pid must not be signalled"
        );
        crate::sys::terminate_tree(pid, Duration::from_secs(2));
        let _ = reaper.join();

        // wait_for_job persists the downgrade.
        let r2 = create_job(d.path(), new_job("rescue", true)).unwrap();
        let id2 = id_of(&r2).to_string();
        update_job(d.path(), &id2, |o| o.set("status", "running")).unwrap();
        let _probe = crate::registry::hold(&id2);
        drop(_probe); // lock file exists, nobody holds it
        let w = wait_for_job(
            d.path(),
            &id2,
            Duration::from_secs(2),
            Duration::from_millis(10),
        )
        .unwrap()
        .unwrap();
        assert_eq!(status_of(&w), "failed");
        assert_eq!(
            w.get("meta")
                .and_then(|m| m.get("livenessDowngradedBy"))
                .and_then(Value::as_str),
            Some("waitForJob")
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_state_dir_is_refused() {
        let d = TempDir::new("agy-jobs-test-").unwrap();
        let elsewhere = TempDir::new("agy-elsewhere-").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), d.path().join(".agy-plugin")).unwrap();
        assert!(create_job(d.path(), new_job("review", false)).is_err());
        assert!(
            std::fs::read_dir(elsewhere.path())
                .unwrap()
                .next()
                .is_none(),
            "nothing written through the link"
        );
    }

    #[test]
    fn wait_returns_running_record_on_timeout_and_none_for_missing() {
        let d = TempDir::new("agy-jobs-test-").unwrap();
        let r = create_job(d.path(), new_job("review", true)).unwrap();
        let id = id_of(&r).to_string();
        let w = wait_for_job(
            d.path(),
            &id,
            Duration::from_millis(50),
            Duration::from_millis(10),
        )
        .unwrap()
        .unwrap();
        assert_eq!(status_of(&w), "pending");
        assert!(
            wait_for_job(
                d.path(),
                "agy-00000000",
                Duration::from_millis(10),
                Duration::from_millis(5)
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn concurrent_updates_are_not_lost() {
        let d = TempDir::new("agy-jobs-test-").unwrap();
        let r = create_job(d.path(), new_job("review", false)).unwrap();
        let id = id_of(&r).to_string();
        update_job(d.path(), &id, |o| o.set("counter", 0i64)).unwrap();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let root = d.path().to_path_buf();
                let id = id.clone();
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        update_job(&root, &id, |o| {
                            let n = o.get("counter").and_then(Value::as_i64).unwrap_or(0);
                            o.set("counter", n + 1);
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let rec = read_job(d.path(), &id).unwrap().unwrap();
        assert_eq!(rec.get("counter").and_then(Value::as_i64), Some(80));
    }

    #[cfg(unix)]
    #[test]
    fn records_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new("agy-jobs-test-").unwrap();
        let r = create_job(d.path(), new_job("review", false)).unwrap();
        let mode = std::fs::metadata(record_path(d.path(), id_of(&r)))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
