//! The per-user record of jobs this user started on this machine.
//!
//! Job records live in the workspace (`.agy-plugin/jobs/*.json`) and a
//! repository can COMMIT such files: a cloned repo could plant a record that
//! names any pid and any directory. So nothing that kills a process or
//! deletes a directory may trust the workspace record. This registry lives
//! outside every repo, in a private (0700) per-user directory, and only
//! this program writes it:
//!
//! - `<id>.json`: the workspace, worker pid, agy child pid and stage dir;
//! - `<id>.lock`: held with an exclusive OS file lock (`File::lock`) by the
//!   worker while it runs. The OS releases it when the process dies, however
//!   it dies, so "the lock is held" proves the worker is alive and is OURS —
//!   a pid alone could have been reused by an unrelated process.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use crate::json::{self, Map, Value};
use crate::util;

/// `$AGY_PLUGIN_STATE_DIR`, else the platform's per-user state dir. Unit
/// tests always use a private per-process temp dir, never the real one.
pub(crate) fn dir() -> Option<PathBuf> {
    #[cfg(test)]
    {
        return Some(
            std::env::temp_dir()
                .join(format!("agy-plugin-test-registry-{}", std::process::id()))
                .join("jobs"),
        );
    }
    #[cfg_attr(test, allow(unreachable_code))]
    if let Some(d) = util::env_nonempty("AGY_PLUGIN_STATE_DIR") {
        return Some(PathBuf::from(d).join("jobs"));
    }
    #[cfg(windows)]
    {
        util::env_nonempty("LOCALAPPDATA").map(|l| PathBuf::from(l).join("agy-plugin").join("jobs"))
    }
    #[cfg(not(windows))]
    {
        let base = util::env_nonempty("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| util::home_dir().map(|h| h.join(".local").join("state")))?;
        Some(base.join("agy-plugin").join("jobs"))
    }
}

fn ensure_dir() -> Option<PathBuf> {
    let d = dir()?;
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(&d).ok()?;
    // Refuse a registry dir that is a symlink or (on Unix) not private.
    let meta = std::fs::symlink_metadata(&d).ok()?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
        }
    }
    Some(d)
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Entry {
    pub(crate) workspace: PathBuf,
    pub(crate) stage_dir: Option<PathBuf>,
    pub(crate) pid: Option<u32>,
    /// The worker leads its own process group (background workers).
    pub(crate) group_leader: bool,
    /// The agy process the worker started, if any.
    pub(crate) child_pid: Option<u32>,
}

fn to_value(e: &Entry) -> Value {
    let mut m = Map::new();
    m.set("workspace", e.workspace.display().to_string());
    m.set(
        "stageDir",
        e.stage_dir.as_ref().map(|p| p.display().to_string()),
    );
    m.set("pid", e.pid);
    m.set("groupLeader", e.group_leader);
    m.set("childPid", e.child_pid);
    Value::Object(m)
}

fn pid_field(v: &Value, k: &str) -> Option<u32> {
    v.get(k)
        .and_then(Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
        .filter(|p| *p > 1)
}

fn from_value(v: &Value) -> Option<Entry> {
    Some(Entry {
        workspace: PathBuf::from(v.get("workspace")?.as_str()?),
        stage_dir: v.get("stageDir").and_then(Value::as_str).map(PathBuf::from),
        pid: pid_field(v, "pid"),
        group_leader: v
            .get("groupLeader")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        child_pid: pid_field(v, "childPid"),
    })
}

fn valid(id: &str) -> bool {
    crate::jobs::is_valid_id(id)
}

/// Record a new job (called when the job is created).
pub(crate) fn register(id: &str, workspace: &Path, stage_dir: Option<&Path>) {
    let e = Entry {
        workspace: util::canonical_or_self(workspace),
        stage_dir: stage_dir.map(util::canonical_or_self),
        ..Entry::default()
    };
    write(id, &e);
}

fn write(id: &str, e: &Entry) {
    if !valid(id) {
        return;
    }
    if let Some(d) = ensure_dir() {
        let text = format!("{}\n", json::to_string_pretty(&to_value(e)));
        let _ = crate::jobs::write_atomic(&d.join(format!("{id}.json")), text.as_bytes());
    }
}

/// The entry for `id`, if THIS user's registry has one.
pub(crate) fn read(id: &str) -> Option<Entry> {
    if !valid(id) {
        return None;
    }
    let text = std::fs::read_to_string(dir()?.join(format!("{id}.json"))).ok()?;
    from_value(&json::parse(&text).ok()?)
}

/// The entry for `id` only if it belongs to `workspace`.
pub(crate) fn read_for(id: &str, workspace: &Path) -> Option<Entry> {
    read(id).filter(|e| e.workspace == util::canonical_or_self(workspace))
}

pub(crate) fn update(id: &str, f: impl FnOnce(&mut Entry)) {
    if let Some(mut e) = read(id) {
        f(&mut e);
        write(id, &e);
    }
}

pub(crate) fn remove(id: &str) {
    if let Some(d) = dir().filter(|_| valid(id)) {
        let _ = std::fs::remove_file(d.join(format!("{id}.json")));
        let _ = std::fs::remove_file(d.join(format!("{id}.lock")));
    }
}

fn open_lock(id: &str) -> Option<File> {
    let d = ensure_dir()?;
    let mut o = OpenOptions::new();
    o.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(d.join(format!("{id}.lock"))).ok()
}

/// Take the job's liveness lock for the lifetime of the returned handle.
pub(crate) fn hold(id: &str) -> Option<File> {
    if !valid(id) {
        return None;
    }
    let f = open_lock(id)?;
    f.lock().ok()?;
    Some(f)
}

/// Some(true) if the job's worker holds its lock (alive and ours),
/// Some(false) if nobody holds it, None if there is no lock to probe.
pub(crate) fn worker_alive(id: &str) -> Option<bool> {
    if !valid(id) {
        return None;
    }
    let path = dir()?.join(format!("{id}.lock"));
    let f = OpenOptions::new().read(true).write(true).open(path).ok()?;
    match f.try_lock() {
        Ok(()) => {
            let _ = f.unlock();
            Some(false)
        }
        Err(TryLockError::WouldBlock) => Some(true),
        Err(TryLockError::Error(_)) => None,
    }
}

/// Is `p` a review stage dir THIS program created (an `agy-review-*`
/// directory directly inside the OS temp dir, not a symlink)? Only such a
/// path may be deleted on cancel.
pub(crate) fn is_our_stage_dir(p: &Path) -> bool {
    let tmp = util::canonical_or_self(&std::env::temp_dir());
    let Ok(meta) = std::fs::symlink_metadata(p) else {
        return false;
    };
    meta.is_dir()
        && !meta.file_type().is_symlink()
        && p.parent().map(util::canonical_or_self).as_deref() == Some(tmp.as_path())
        && p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("agy-review-"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::TempDir;

    #[test]
    fn register_read_and_workspace_binding() {
        let ws = TempDir::new("agy-ws-").unwrap();
        register("agy-0000aaaa", ws.path(), None);
        let e = read("agy-0000aaaa").unwrap();
        assert_eq!(e.workspace, util::canonical_or_self(ws.path()));
        assert!(read_for("agy-0000aaaa", ws.path()).is_some());
        let other = TempDir::new("agy-ws-").unwrap();
        assert!(
            read_for("agy-0000aaaa", other.path()).is_none(),
            "bound to its workspace"
        );
        update("agy-0000aaaa", |e| {
            e.pid = Some(4242);
            e.child_pid = Some(1); // pid 1 is never accepted
        });
        let e = read("agy-0000aaaa").unwrap();
        assert_eq!(e.pid, Some(4242));
        assert_eq!(e.child_pid, None);
        assert!(read("../../etc/passwd").is_none());
        remove("agy-0000aaaa");
        assert!(read("agy-0000aaaa").is_none());
    }

    #[test]
    fn lock_proves_liveness() {
        assert_eq!(worker_alive("agy-0000bbbb"), None, "no lock file yet");
        let held = hold("agy-0000bbbb").unwrap();
        assert_eq!(worker_alive("agy-0000bbbb"), Some(true));
        drop(held);
        assert_eq!(worker_alive("agy-0000bbbb"), Some(false));
    }

    #[cfg(unix)]
    #[test]
    fn registry_dir_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let ws = TempDir::new("agy-ws-").unwrap();
        register("agy-0000cccc", ws.path(), None);
        let mode = std::fs::metadata(dir().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn only_our_stage_dirs_qualify_for_deletion() {
        let ours = TempDir::new("agy-review-").unwrap();
        assert!(is_our_stage_dir(ours.path()));
        let other = TempDir::new("agy-run-").unwrap();
        assert!(!is_our_stage_dir(other.path()));
        let home = util::home_dir().unwrap_or_default();
        assert!(!is_our_stage_dir(&home));
        assert!(!is_our_stage_dir(Path::new("/")));
        let nested = ours.path().join("agy-review-x");
        std::fs::create_dir(&nested).unwrap();
        assert!(
            !is_our_stage_dir(&nested),
            "must sit directly in the temp dir"
        );
        #[cfg(unix)]
        {
            let link = std::env::temp_dir().join(format!("agy-review-link{}", util::random_hex(4)));
            std::os::unix::fs::symlink(&home, &link).unwrap();
            assert!(!is_our_stage_dir(&link), "a symlink is never ours");
            std::fs::remove_file(&link).unwrap();
        }
    }
}
