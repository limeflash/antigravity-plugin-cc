//! git plumbing: review diffs, staging review materials for agy (Design A+),
//! and the worktree / clean-tree helpers behind /agy:rescue.

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::prompts::Omitted;
use crate::util::{self, Captured, Fail};

const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// Largest changed file copied into the review stage dir. Staged files are
/// read by agy from disk (not put in argv), so this is far above the old
/// inline budget; it only stops generated blobs.
pub(crate) const STAGE_FILE_MAX_BYTES: u64 = 2 * 1024 * 1024;

fn git(dir: &Path, args: &[&str]) -> Result<Captured, Fail> {
    let mut cmd = Command::new("git");
    // core.fsmonitor can name a program to run on status/diff; a repo's own
    // config (or an extracted archive's .git/config) must not get to run
    // code through us.
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(dir);
    util::run_captured(&mut cmd, Some(GIT_TIMEOUT)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Fail::runtime("error: git not found on PATH. The review and rescue commands need git.")
        } else {
            Fail::runtime(format!("error: could not run git: {e}"))
        }
    })
}

/// `git rev-parse --show-toplevel`, or None outside a repo.
pub(crate) fn root(dir: &Path) -> Option<PathBuf> {
    let out = git(dir, &["rev-parse", "--show-toplevel"]).ok()?;
    let s = out.stdout_str();
    let s = s.trim();
    (out.ok() && !s.is_empty()).then(|| PathBuf::from(s))
}

fn resolve_ref(dir: &Path, r: &str) -> Option<String> {
    // `--end-of-options` stops a ref starting with `-` from being read as an
    // option (`--base=--output=/tmp/x`).
    let out = git(
        dir,
        &["rev-parse", "--verify", "--quiet", "--end-of-options", r],
    )
    .ok()?;
    let s = out.stdout_str().trim().to_string();
    (out.ok() && !s.is_empty()).then_some(s)
}

fn context_lines() -> u32 {
    util::env_nonempty("AGY_REVIEW_CONTEXT")
        .and_then(|v| {
            let digits: String = v
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            digits.parse::<u32>().ok()
        })
        .unwrap_or(25)
        .min(10_000)
}

/// Paths from a `-z` listing (NUL-separated, never quoted or trimmed, so
/// names with quotes, newlines or edge spaces survive).
fn name_list(out: &Captured) -> Vec<String> {
    if !out.ok() {
        return Vec::new();
    }
    out.stdout_str()
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// What a review looks at.
pub(crate) struct DiffContext {
    pub(crate) scope: crate::prompts::Scope,
    pub(crate) diff: String,
    pub(crate) files: Vec<String>,
    pub(crate) root: PathBuf,
}

/// Uncommitted changes against HEAD (falls back to the unstaged diff in a
/// repo with no commits yet). Untracked files are not included.
pub(crate) fn working_tree_diff(cwd: &Path) -> Result<DiffContext, Fail> {
    let root = root(cwd)
        .ok_or_else(|| Fail::runtime(format!("error: not in a git repo: {}", cwd.display())))?;
    let u = format!("-U{}", context_lines());
    // -c core.quotePath=false: report non-ASCII paths verbatim so they can be
    // found on disk; --no-ext-diff/--no-color: ignore user diff config.
    let base = [
        "-c",
        "core.quotePath=false",
        "diff",
        "--no-ext-diff",
        "--no-color",
        "--no-textconv",
    ];
    let mut args: Vec<&str> = base.to_vec();
    args.extend([u.as_str(), "HEAD"]);
    let mut diff = git(&root, &args)?.stdout_str();
    let mut range: Vec<&str> = vec!["HEAD"];
    if diff.trim().is_empty() {
        let mut args: Vec<&str> = base.to_vec();
        args.push(u.as_str());
        diff = git(&root, &args)?.stdout_str();
        range.clear();
    }
    let mut names: Vec<&str> = vec![
        "-c",
        "core.quotePath=false",
        "diff",
        "--no-ext-diff",
        "--name-only",
        "-z",
    ];
    names.extend(range);
    let files = name_list(&git(&root, &names)?);
    Ok(DiffContext {
        scope: crate::prompts::Scope::WorkingTree,
        diff,
        files,
        root,
    })
}

/// HEAD compared against the merge-base of HEAD and `base`.
pub(crate) fn branch_diff(cwd: &Path, base: &str) -> Result<DiffContext, Fail> {
    let root = root(cwd)
        .ok_or_else(|| Fail::runtime(format!("error: not in a git repo: {}", cwd.display())))?;
    let base_sha = resolve_ref(&root, base).ok_or_else(|| {
        Fail::runtime(format!(
            "error: cannot resolve --base ref '{base}' to a commit"
        ))
    })?;
    let head_sha = resolve_ref(&root, "HEAD")
        .ok_or_else(|| Fail::runtime("error: HEAD is missing or detached and unresolvable"))?;
    let mb = git(&root, &["merge-base", &base_sha, &head_sha])?;
    let compare = if mb.ok() && !mb.stdout_str().trim().is_empty() {
        mb.stdout_str().trim().to_string()
    } else {
        base_sha
    };
    let range = format!("{compare}...HEAD");
    let u = format!("-U{}", context_lines());
    let diff = git(
        &root,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--no-textconv",
            &u,
            &range,
        ],
    )?
    .stdout_str();
    let files = name_list(&git(
        &root,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-ext-diff",
            "--name-only",
            "-z",
            &range,
        ],
    )?);
    Ok(DiffContext {
        scope: crate::prompts::Scope::Branch {
            base: base.to_string(),
            merge_base: Some(compare),
        },
        diff,
        files,
        root,
    })
}

/// A repo-relative path from git that stays inside the stage dir: no
/// absolute paths, no `..`, no drive prefixes.
fn safe_relative(rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel);
    if rel.is_empty() || p.is_absolute() || rel.starts_with('/') || rel.starts_with('\\') {
        return None;
    }
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

#[derive(Default)]
pub(crate) struct Staged {
    pub(crate) staged: Vec<String>,
    pub(crate) omitted: Vec<Omitted>,
}

/// Design A+: write the full diff and the full current content of each
/// changed file under `stage_dir` (diff.patch, files/<rel>) so agy reads
/// them from disk — no argv size limit, whole-file context, and the repo
/// itself is never in agy's workspace. Skips anything that could smuggle
/// in a file from outside the repo.
pub(crate) fn stage_review_materials(
    stage_dir: &Path,
    root: &Path,
    files: &[String],
    diff: &str,
) -> Result<Staged, Fail> {
    let io =
        |e: std::io::Error| Fail::runtime(format!("error: could not stage review materials: {e}"));
    std::fs::create_dir_all(stage_dir).map_err(io)?;
    std::fs::write(stage_dir.join("diff.patch"), diff).map_err(io)?;
    let files_root = stage_dir.join("files");
    let real_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut res = Staged::default();
    for rel in files {
        let omit = |reason: String| Omitted {
            path: rel.clone(),
            reason,
        };
        let Some(rel_path) = safe_relative(rel) else {
            res.omitted.push(omit(if Path::new(rel).is_absolute() {
                "absolute path (skipped)".into()
            } else {
                "path escapes stage dir (skipped)".into()
            }));
            continue;
        };
        let abs = root.join(&rel_path);
        let meta = match std::fs::symlink_metadata(&abs) {
            Ok(m) => m,
            Err(_) => {
                res.omitted.push(omit("missing/deleted".into()));
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            res.omitted.push(omit("symlink (skipped)".into()));
            continue;
        }
        if !meta.is_file() {
            res.omitted.push(omit("not a regular file".into()));
            continue;
        }
        if meta.len() > STAGE_FILE_MAX_BYTES {
            res.omitted
                .push(omit(format!("too large ({} bytes)", meta.len())));
            continue;
        }
        // A parent dir can be a symlink pointing outside the repo
        // (linked_dir/secret.txt): the real path must stay under the root.
        match std::fs::canonicalize(&abs) {
            Ok(real) if real.starts_with(&real_root) && real != real_root => {}
            Ok(_) => {
                res.omitted.push(omit("resolves outside the repo".into()));
                continue;
            }
            Err(_) => {
                res.omitted.push(omit("unresolvable path".into()));
                continue;
            }
        }
        let bytes = match std::fs::read(&abs) {
            Ok(b) => b,
            Err(_) => {
                res.omitted.push(omit("unreadable".into()));
                continue;
            }
        };
        if bytes.contains(&0) {
            res.omitted.push(omit("binary".into()));
            continue;
        }
        let dest = files_root.join(&rel_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        std::fs::write(&dest, String::from_utf8_lossy(&bytes).as_bytes()).map_err(io)?;
        res.staged.push(rel.clone());
    }
    Ok(res)
}

/// Does the repository COMMIT `.agy-plugin/`? Then its job records came
/// with the clone and were not created by this user.
pub(crate) fn tracks_state_dir(root: &Path) -> bool {
    git(root, &["ls-files", "-z", "--", crate::jobs::STATE_DIR])
        .map(|o| o.ok() && !o.stdout.is_empty())
        .unwrap_or(false)
}

pub(crate) fn is_repo(dir: &Path) -> bool {
    root(dir).is_some()
}

/// Clean = `git status --porcelain` printed nothing. A git failure counts as
/// dirty (fail closed: rescue refuses).
pub(crate) fn is_clean(dir: &Path) -> bool {
    match git(dir, &["status", "--porcelain"]) {
        Ok(out) => out.ok() && out.stdout_str().trim().is_empty(),
        Err(_) => false,
    }
}

/// `git diff --stat HEAD` plus untracked files, for after a rescue run.
pub(crate) fn change_summary(dir: &Path) -> String {
    let stat = git(dir, &["diff", "--no-ext-diff", "--stat", "HEAD"])
        .map(|o| o.stdout_str().trim().to_string())
        .unwrap_or_default();
    let untracked = git(
        dir,
        &[
            "-c",
            "core.quotePath=false",
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
        ],
    )
    .map(|o| name_list(&o))
    .unwrap_or_default();
    let mut lines = Vec::new();
    if !stat.is_empty() {
        lines.push(stat);
    }
    if !untracked.is_empty() {
        lines.push("new (untracked) files:".to_string());
        lines.extend(untracked.iter().map(|f| format!("  + {f}")));
    }
    if lines.is_empty() {
        "(no changes detected)".to_string()
    } else {
        lines.join("\n")
    }
}

/// A separate checkout of HEAD for `rescue --isolate`: `git clone --shared`
/// into `dir`, detached at the same commit. Unlike a linked worktree, the
/// clone has its own `.git` (config, hooks, refs), so edits there can't
/// reach the real repository's metadata. Objects are shared read-only via
/// alternates, so it is fast.
pub(crate) fn isolated_checkout(root: &Path, dir: &Path) -> Result<(), String> {
    let head = resolve_ref(root, "HEAD").ok_or("HEAD is missing (no commits yet?)")?;
    let d = dir.display().to_string();
    let src = root.display().to_string();
    let first_line = |o: &Captured| {
        let e = o.stderr_str();
        if e.trim().is_empty() {
            o.stdout_str().trim().to_string()
        } else {
            e.trim().to_string()
        }
    };
    let out = git(
        root,
        &[
            "clone",
            "--quiet",
            "--shared",
            "--no-checkout",
            "--",
            &src,
            &d,
        ],
    )
    .map_err(|f| f.message)?;
    if !out.ok() {
        return Err(first_line(&out));
    }
    let out = git(dir, &["checkout", "--quiet", "--detach", &head]).map_err(|f| f.message)?;
    if !out.ok() {
        return Err(first_line(&out));
    }
    Ok(())
}

pub(crate) fn remove_isolated(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// Stage everything in the isolated checkout and return (`--stat`, patch).
pub(crate) fn isolated_patch(wt: &Path) -> (String, String) {
    let _ = git(wt, &["add", "-A"]);
    let stat = git(wt, &["diff", "--no-ext-diff", "--cached", "--stat", "HEAD"])
        .map(|o| o.stdout_str().trim().to_string())
        .unwrap_or_default();
    let patch = git(
        wt,
        &[
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--no-textconv",
            "--binary",
            "--cached",
            "HEAD",
        ],
    )
    .map(|o| o.stdout_str())
    .unwrap_or_default();
    (stat, patch)
}

/// Keep `.agy-plugin/` (job state) out of `git status`, so a later rescue
/// doesn't see its own state as a dirty tree. Uses `git rev-parse
/// --git-path` so it also works in linked worktrees, where `.git` is a file
/// (0.8.x silently skipped those).
pub(crate) fn ensure_exclude(root: &Path) {
    let Ok(out) = git(root, &["rev-parse", "--git-path", "info/exclude"]) else {
        return;
    };
    if !out.ok() {
        return;
    }
    let p = PathBuf::from(out.stdout_str().trim());
    let p = if p.is_absolute() { p } else { root.join(p) };
    let current = std::fs::read_to_string(&p).unwrap_or_default();
    if current
        .lines()
        .any(|l| matches!(l.trim(), ".agy-plugin" | ".agy-plugin/"))
    {
        return;
    }
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let sep = if !current.is_empty() && !current.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    let _ = std::fs::write(
        &p,
        format!(
            "{current}{sep}# Added by the agy plugin — local job-state dir (not committed).\n.agy-plugin/\n"
        ),
    );
}

#[cfg(test)]
pub(crate) mod test_repo {
    use std::path::Path;
    use std::process::Command;

    pub(crate) fn sh_git(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            st.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&st.stderr)
        );
    }

    /// A repo with one commit containing `a.txt`.
    pub(crate) fn init(dir: &Path) {
        sh_git(dir, &["init", "-q", "-b", "main"]);
        sh_git(dir, &["config", "user.email", "t@t.t"]);
        sh_git(dir, &["config", "user.name", "t"]);
        sh_git(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        sh_git(dir, &["add", "-A"]);
        sh_git(dir, &["commit", "-qm", "init"]);
    }
}

#[cfg(test)]
mod tests {
    use super::test_repo::*;
    use super::*;
    use crate::util::TempDir;

    #[test]
    fn root_and_repo_detection() {
        let d = TempDir::new("agy-git-test-").unwrap();
        assert!(!is_repo(d.path()));
        init(d.path());
        assert!(is_repo(d.path()));
        let r = std::fs::canonicalize(root(d.path()).unwrap()).unwrap();
        assert_eq!(r, std::fs::canonicalize(d.path()).unwrap());
    }

    #[test]
    fn working_tree_diff_sees_edits_not_untracked() {
        let d = TempDir::new("agy-git-test-").unwrap();
        init(d.path());
        let ctx = working_tree_diff(d.path()).unwrap();
        assert!(ctx.diff.trim().is_empty());
        std::fs::write(d.path().join("a.txt"), "two\n").unwrap();
        std::fs::write(d.path().join("new.txt"), "x\n").unwrap();
        let ctx = working_tree_diff(d.path()).unwrap();
        assert!(ctx.diff.contains("-one") && ctx.diff.contains("+two"));
        assert_eq!(ctx.files, vec!["a.txt"]);
        assert!(
            working_tree_diff(&std::env::temp_dir().join("definitely-not-a-repo-xyz")).is_err()
        );
    }

    #[test]
    fn branch_diff_uses_the_merge_base() {
        let d = TempDir::new("agy-git-test-").unwrap();
        init(d.path());
        sh_git(d.path(), &["checkout", "-q", "-b", "feat"]);
        std::fs::write(d.path().join("b.txt"), "feature\n").unwrap();
        sh_git(d.path(), &["add", "-A"]);
        sh_git(d.path(), &["commit", "-qm", "feat"]);
        let ctx = branch_diff(d.path(), "main").unwrap();
        assert!(ctx.diff.contains("+feature"));
        assert_eq!(ctx.files, vec!["b.txt"]);
        assert!(
            branch_diff(d.path(), "no-such-ref")
                .err()
                .unwrap()
                .message
                .contains("cannot resolve")
        );
        // A ref that looks like an option must not reach git as one.
        assert!(branch_diff(d.path(), "--output=/tmp/agy-pwned").is_err());
        assert!(!Path::new("/tmp/agy-pwned").exists());
    }

    #[test]
    fn stage_writes_diff_and_full_files() {
        let d = TempDir::new("agy-git-test-").unwrap();
        init(d.path());
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("sub/c.rs"), "fn main() {}\n").unwrap();
        let st = TempDir::new("agy-stage-test-").unwrap();
        let r = stage_review_materials(
            st.path(),
            d.path(),
            &["a.txt".into(), "sub/c.rs".into(), "gone.txt".into()],
            "DIFF",
        )
        .unwrap();
        assert_eq!(r.staged, vec!["a.txt", "sub/c.rs"]);
        assert_eq!(
            std::fs::read_to_string(st.path().join("diff.patch")).unwrap(),
            "DIFF"
        );
        assert_eq!(
            std::fs::read_to_string(st.path().join("files/sub/c.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert_eq!(r.omitted[0].reason, "missing/deleted");
    }

    #[cfg(unix)]
    #[test]
    fn stage_refuses_symlinks_dir_escapes_traversal_and_binaries() {
        let d = TempDir::new("agy-git-test-").unwrap();
        init(d.path());
        let outside = TempDir::new("agy-outside-").unwrap();
        std::fs::write(outside.path().join("secret.txt"), "TOP SECRET").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", d.path().join("pwned")).unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("linkdir")).unwrap();
        std::fs::write(d.path().join("bin.dat"), [1u8, 0, 2]).unwrap();
        let st = TempDir::new("agy-stage-test-").unwrap();
        let files: Vec<String> = [
            "pwned",
            "linkdir/secret.txt",
            "../../etc/passwd",
            "/etc/passwd",
            "bin.dat",
            "a.txt",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let r = stage_review_materials(st.path(), d.path(), &files, "").unwrap();
        assert_eq!(r.staged, vec!["a.txt"]);
        let reasons: Vec<&str> = r.omitted.iter().map(|o| o.reason.as_str()).collect();
        assert_eq!(
            reasons,
            vec![
                "symlink (skipped)",
                "resolves outside the repo",
                "path escapes stage dir (skipped)",
                "absolute path (skipped)",
                "binary"
            ]
        );
        // Nothing from outside the repo landed anywhere in the stage dir.
        let mut stack = vec![st.path().to_path_buf()];
        while let Some(p) = stack.pop() {
            for e in std::fs::read_dir(&p).unwrap() {
                let e = e.unwrap().path();
                if e.is_dir() {
                    stack.push(e);
                } else {
                    let c = std::fs::read_to_string(&e).unwrap_or_default();
                    assert!(
                        !c.contains("TOP SECRET") && !c.contains("root:"),
                        "{}",
                        e.display()
                    );
                }
            }
        }
    }

    #[test]
    fn clean_tree_summary_and_exclude() {
        let d = TempDir::new("agy-git-test-").unwrap();
        init(d.path());
        assert!(is_clean(d.path()));
        assert_eq!(change_summary(d.path()), "(no changes detected)");
        std::fs::create_dir(d.path().join(".agy-plugin")).unwrap();
        std::fs::write(d.path().join(".agy-plugin/x.json"), "{}").unwrap();
        assert!(!is_clean(d.path()), "state dir shows until excluded");
        ensure_exclude(d.path());
        ensure_exclude(d.path());
        assert!(is_clean(d.path()));
        let ex = std::fs::read_to_string(d.path().join(".git/info/exclude")).unwrap();
        assert_eq!(ex.matches(".agy-plugin/").count(), 1, "idempotent");
        std::fs::write(d.path().join("a.txt"), "changed\n").unwrap();
        std::fs::write(d.path().join("n.txt"), "new\n").unwrap();
        let s = change_summary(d.path());
        assert!(s.contains("a.txt") && s.contains("new (untracked) files:\n  + n.txt"));
    }

    #[test]
    fn exclude_works_in_a_linked_worktree() {
        let d = TempDir::new("agy-git-test-").unwrap();
        init(d.path());
        let wt_parent = TempDir::new("agy-wt-").unwrap();
        let wt = wt_parent.path().join("wt");
        sh_git(
            d.path(),
            &["worktree", "add", "--detach", wt.to_str().unwrap(), "HEAD"],
        );
        assert!(
            wt.join(".git").is_file(),
            ".git is a gitfile in a linked worktree"
        );
        std::fs::create_dir(wt.join(".agy-plugin")).unwrap();
        std::fs::write(wt.join(".agy-plugin/x"), "1").unwrap();
        ensure_exclude(&wt);
        assert!(is_clean(&wt));
        sh_git(
            d.path(),
            &["worktree", "remove", "--force", wt.to_str().unwrap()],
        );
    }

    #[test]
    fn isolated_checkout_round_trip_leaves_real_repo_untouched() {
        let d = TempDir::new("agy-git-test-").unwrap();
        init(d.path());
        let parent = TempDir::new("agy-iso-").unwrap();
        let wt = parent.path().join("wt");
        isolated_checkout(d.path(), &wt).unwrap();
        assert!(
            wt.join(".git").is_dir(),
            "a clone has its own .git, not a gitfile"
        );
        std::fs::write(wt.join("a.txt"), "isolated\n").unwrap();
        std::fs::write(wt.join("added.txt"), "x\n").unwrap();
        // Metadata written in the checkout stays there.
        sh_git(&wt, &["config", "agy.marker", "1"]);
        let (stat, patch) = isolated_patch(&wt);
        assert!(stat.contains("a.txt") && stat.contains("added.txt"));
        assert!(patch.contains("+isolated"));
        remove_isolated(&wt);
        assert!(!wt.exists());
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap(),
            "one\n"
        );
        assert!(is_clean(d.path()));
        let cfg = std::fs::read_to_string(d.path().join(".git/config")).unwrap();
        assert!(!cfg.contains("marker"), "real repo config untouched");
        let wl = std::process::Command::new("git")
            .arg("-C")
            .arg(d.path())
            .args(["worktree", "list"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&wl.stdout).lines().count(),
            1,
            "no linked worktree registered"
        );
    }

    #[test]
    fn odd_file_names_survive_the_name_list() {
        let d = TempDir::new("agy-git-test-").unwrap();
        init(d.path());
        let odd = if cfg!(windows) {
            "sp ace.txt"
        } else {
            " edge\"quote\\.txt"
        };
        std::fs::write(d.path().join(odd), "1\n").unwrap();
        sh_git(d.path(), &["add", "-A"]);
        sh_git(d.path(), &["commit", "-qm", "odd"]);
        std::fs::write(d.path().join(odd), "2\n").unwrap();
        let ctx = working_tree_diff(d.path()).unwrap();
        assert_eq!(ctx.files, vec![odd.to_string()]);
    }

    #[test]
    fn safe_relative_rejects_escapes() {
        assert!(safe_relative("a/b.txt").is_some());
        assert!(safe_relative("./a").is_some());
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            "\\\\server\\share",
            "a/..",
        ] {
            assert!(safe_relative(bad).is_none(), "{bad}");
        }
    }
}
