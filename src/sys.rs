//! OS calls std does not wrap: is a pid alive, stop a process tree, and
//! detach a background worker. Declared with `extern` against the C library
//! (Unix) and kernel32 (Windows), both of which every Rust binary already
//! links — no crates.

use std::process::Command;
use std::time::{Duration, Instant};

#[cfg(unix)]
mod imp {
    use std::os::raw::c_int;

    unsafe extern "C" {
        fn kill(pid: c_int, sig: c_int) -> c_int;
    }

    // Same values on Linux and macOS.
    pub(super) const SIGTERM: c_int = 15;
    pub(super) const SIGKILL: c_int = 9;
    const EPERM: i32 = 1;

    /// kill(2) on `pid` (negative = whole process group). Returns Ok on
    /// success, Err(errno) otherwise.
    pub(super) fn send(pid: i64, sig: c_int) -> Result<(), i32> {
        let Ok(p) = c_int::try_from(pid) else {
            return Err(0);
        };
        // SAFETY: kill(2) takes plain integers and has no memory effects.
        let rc = unsafe { kill(p, sig) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
        }
    }

    pub(super) fn alive(pid: u32) -> bool {
        match send(i64::from(pid), 0) {
            Ok(()) => !is_zombie(pid),
            // The process exists but belongs to someone else.
            Err(e) => e == EPERM,
        }
    }

    /// kill(pid, 0) succeeds on a zombie (exited, not yet reaped). An
    /// orphaned worker is reaped by whatever subreaper adopted it, which on
    /// CI runners can take a while; it is gone for our purposes. Linux only
    /// (/proc); elsewhere launchd/init reap promptly.
    fn is_zombie(pid: u32) -> bool {
        #[cfg(target_os = "linux")]
        {
            if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                // "pid (comm) S ..." — comm may contain spaces/parens, so
                // take the state after the LAST ')'.
                if let Some(rest) = stat.rsplit_once(')') {
                    return rest.1.trim_start().starts_with('Z');
                }
            }
            false
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = pid;
            false
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    type Handle = *mut c_void;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_ACCESS_DENIED: i32 = 5;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    pub(super) fn alive(pid: u32) -> bool {
        // SAFETY: plain Win32 calls; the handle is closed on every path.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                // Exists but we may not query it.
                return std::io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED);
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            ok && code == STILL_ACTIVE
        }
    }
}

/// True if a process with this pid exists (pid 0 and absurd values: false).
pub(crate) fn process_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    imp::alive(pid)
}

/// Stop a background worker AND everything it started (agy, its tools).
/// Workers are started as process-group leaders (see `detach`), so on Unix
/// we signal the group: SIGTERM, wait up to `grace`, then SIGKILL. On
/// Windows `taskkill /T` walks the process tree. Returns true if the worker
/// is gone afterwards.
pub(crate) fn terminate_tree(pid: u32, grace: Duration) -> bool {
    if !process_alive(pid) {
        return true;
    }
    #[cfg(unix)]
    {
        let group = -i64::from(pid);
        // Fall back to the pid alone if it is not a group leader (a record
        // from an older plugin version).
        if imp::send(group, imp::SIGTERM).is_err() {
            let _ = imp::send(i64::from(pid), imp::SIGTERM);
        }
        if wait_gone(pid, grace) {
            return true;
        }
        if imp::send(group, imp::SIGKILL).is_err() {
            let _ = imp::send(i64::from(pid), imp::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let p = pid.to_string();
        let _ = crate::util::run_simple(
            std::path::Path::new("taskkill"),
            &["/PID", p.as_str(), "/T"],
            Duration::from_secs(10),
        );
        if wait_gone(pid, grace) {
            return true;
        }
        let _ = crate::util::run_simple(
            std::path::Path::new("taskkill"),
            &["/PID", p.as_str(), "/T", "/F"],
            Duration::from_secs(10),
        );
    }
    wait_gone(pid, Duration::from_millis(500))
}

fn wait_gone(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if !process_alive(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Configure `cmd` to outlive its parent: its own process group on Unix (so
/// a Ctrl+C in the user's terminal doesn't reach it, and `terminate_tree`
/// can signal the whole group), a detached console-less process on Windows.
pub(crate) fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_is_alive_and_nonsense_pids_are_not() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(0));
        assert!(!process_alive(u32::MAX));
    }

    #[cfg(unix)]
    #[test]
    fn terminate_tree_kills_a_detached_group_including_grandchildren() {
        // sh (group leader) starts a grandchild sleep; both must die.
        let dir = crate::util::TempDir::new("agy-sys-test-").unwrap();
        let pidfile = dir.path().join("grandchild.pid");
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            &format!("sleep 60 & echo $! > '{}'; wait", pidfile.display()),
        ]);
        detach(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let pid = child.id();
        // Reap concurrently: the killed shell is OUR child, and kill(pid, 0)
        // reports an unreaped zombie as alive. (In production the canceller
        // is a different process, so the worker is reaped by init.)
        let reaper = std::thread::spawn(move || child.wait());
        let deadline = Instant::now() + Duration::from_secs(5);
        let grandchild: u32 = loop {
            if let Ok(s) = std::fs::read_to_string(&pidfile)
                && let Ok(p) = s.trim().parse()
            {
                break p;
            }
            assert!(Instant::now() < deadline, "grandchild never started");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(process_alive(grandchild));
        assert!(terminate_tree(pid, Duration::from_secs(3)));
        let _ = reaper.join();
        assert!(
            wait_gone(grandchild, Duration::from_secs(3)),
            "grandchild orphaned"
        );
    }

    #[test]
    fn terminate_tree_on_dead_pid_is_true() {
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", "exit 0"]);
            c
        } else {
            Command::new("true")
        };
        let mut child = cmd.spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(terminate_tree(pid, Duration::from_millis(100)));
    }
}
