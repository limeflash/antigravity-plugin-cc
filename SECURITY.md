# Security Policy

## Advisory: read-only escape with agy 1.5.7+ (fixed in 0.9.0)

**Affected:** plugin ≤ 0.8.1 used with agy 1.5.7 or newer.
**Impact:** `/agy:ask`, `/agy:review`, `/agy:adversarial-review`,
`/agy:scrape` and `/agy:doc-to-md` were documented as read-only, but agy could
write files outside its workspace — including files in your repository —
when its prompt (or content it read, e.g. a reviewed diff) told it to.

agy 1.5.7 made **Auto** the default agent mode for headless `-p` runs, and in
agy 1.5.16 Auto mode approves, without asking, edits to any file inside a
git/jj/hg repository and any create-or-append elsewhere. 0.8.x relied on agy
running from a temp dir "with no path to the repo"; an absolute path in the
prompt is enough. We reproduced it on agy 1.5.16: plugin 0.8.1's `/agy:ask`
overwrote a file in an unrelated git repository. 0.9.0 pins every read-only
run to agy's Default mode (below) and refuses to run if it can't.

**Upgrade to 0.9.0.** If you can't, avoid the read-only commands on
untrusted content.

## How the read-only commands stay read-only

Applies to ask, review, adversarial-review, scrape and doc-to-md. Each agy run:

1. **runs outside your repo** — its cwd and only `--add-dir` is a fresh
   private (0700) temp dir, or for reviews a stage dir holding the diff and
   copies of the changed files;
2. **is pinned read-only** with one session-only setting (never saved),
   passed as a single last token so no other flag can swallow or override it:
   `--settings={"agentMode":"","permissions":{"deny":["command(*)","write_file(*)"]}}`.
   That is three independent layers: Default agent mode, in which a headless
   run *denies* anything needing approval (writes, edits, shell commands, and
   — verified on agy 1.5.16 — reading files outside its workspace); plus
   explicit deny rules for every shell command and every file write, which
   agy enforces even in Auto mode and which beat any allow rule saved in your
   agy settings. Denied attempts come back as `denied_actions` and the plugin
   shows each one ("agy tried to use X, which this read-only command
   blocks"). agy is also told in the prompt that the session is read-only,
   so it usually doesn't try;
3. **fails closed**: if agy is 1.5.7+ (Auto by default) but has no
   `--settings` flag, the plugin refuses to run instead of running unpinned;
4. runs **without `--dangerously-skip-permissions`**, under `--sandbox`;
5. gets an environment with repo-location hints removed
   (`CLAUDE_PROJECT_DIR`, `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`,
   `GIT_COMMON_DIR`, `AGY_JOB_WORKSPACE`, `PWD`, `OLDPWD`);
6. only accepts `--print-timeout` and `--effort` as extra agy flags, each
   with a value that isn't itself a flag; anything that could widen access
   (`--mode`, `--settings`, `--add-dir`, `--dangerously-skip-permissions`, …)
   is refused.

What enforces step 2 is agy's own permission system, on every platform; the
plugin's tests check that the flags are passed, and `scripts/e2e-live.sh`
re-runs the write-escape attempt against a real agy. If Google changes what
Default mode means, this guarantee changes with it — the e2e script is how
we notice.

**Inputs are guarded before agy sees them:**

- **review** — *every line* of the diff (added, removed and context — a
  `git rm .env` still sends the deleted secrets) *and* the full content of
  every changed file are scanned for credentials (AWS, GitHub, Slack, Anthropic, OpenAI keys, PEM
  blocks, inline `password=` style assignments); a hit aborts with exit 65
  unless `AGY_REVIEW_ALLOW_SECRETS=1`. Best-effort pattern matching, not a
  replacement for gitleaks/trufflehog. Changed files are staged only if they
  are regular files that really live inside the repo (no symlinks, no
  symlinked directories, no `..`, no absolute paths, no binaries).
- **scrape** — the plugin downloads the page itself, then agy only converts
  the saved copy. The URL must be http/https with no credentials, odd
  characters or blocked hosts (loopback, private, link-local, CGNAT,
  multicast, reserved, cloud metadata, `*.localhost`, `*.internal`, single-
  label names, every decimal/hex/octal/IPv6-mapped/NAT64/6to4/`nip.io`
  spelling of those). **Every address the host resolves to** is checked,
  and curl is pinned to the vetted address (`--resolve`), so DNS rebinding
  can't redirect the request; redirects are followed by hand and each hop is
  re-checked; proxies are bypassed unless `AGY_SCRAPE_USE_PROXY=1` (a proxy
  resolves names itself, which would undo the address check); size, time
  and content type are capped.
- **doc-to-md** — allow-listed document extensions only, regular files only,
  10 MB cap, refused under `~/.ssh`, `~/.gnupg`, `~/.aws`,
  `~/.config/gcloud`, `~/.gemini`, `~/.kube`, `~/.docker`, `~/.m2`,
  `~/.purple`, FileZilla, `~/.password-store`, keyrings, browser and mail
  profiles, `~/Library/Keychains`, `%APPDATA%` credential stores, `/etc`,
  `/root`, `/proc`, `/sys`, `/dev`, `/var/run` (refused outright if the home
  directory can't be determined; symlinks resolved; macOS
  `/etc → /private/etc` handled); copied into agy's workspace under a fixed
  name. The copy opens the file with `O_NOFOLLOW`, so a symlink swapped in
  after validation is refused (swapping in a *different regular file* in
  that window is not detected — it needs write access to the path anyway).

## Write-capable commands

| Command | agy may write | Auto-approve | Rails |
|---|---|---|---|
| ask, review, adversarial-review, scrape, doc-to-md | nothing | no | see above |
| `/agy:image` | anywhere (agy ≥ 1.5.2 approves outside the workspace under the flag) | yes | runs from a temp dir; only an image inside `~/.gemini/antigravity-cli/{brain,scratch,cache}` is ever copied to `--output` |
| `/agy:rescue` | the repo (by design) | yes | refuses a dirty git tree (`--allow-dirty` overrides); prints what changed; `--isolate` runs in a separate `git clone --shared` checkout (its own `.git`, repo-location env removed) and saves a patch instead |

`--isolate` keeps agy's *intended* edits out of your tree; it is not a
sandbox — with auto-approval agy could still write elsewhere if a prompt
told it to.

`image` needs auto-approval because agy's `generate_image` flow runs a
command that Default mode denies (checked on agy 1.5.16). Treat its
description like any prompt you give an agent with write access.

## Job control

Job records live in the workspace (`.agy-plugin/`), and a repository could
commit some. So `/agy:cancel` never takes a pid or a path from them: it uses
a private per-user registry outside every repo (`~/.local/state/agy-plugin`,
`%LOCALAPPDATA%\agy-plugin` on Windows, mode 0700). A running worker holds
an exclusive OS file lock there; cancel signals only when that lock is held
(the worker is alive *and* ours — a reused pid can't be hit), stops the
worker's process group and the agy it started, and deletes only a review
stage dir that this program created in the temp dir. Records in a repo that
commits `.agy-plugin/` trigger a warning; a symlinked `.agy-plugin` is
refused. Job records and logs are created 0600.

## The plugin binary

- **std-only Rust, zero third-party crates** — CI fails if a dependency
  appears. OS calls go through `extern` declarations in `src/sys.rs`.
- **Built by GitHub Actions** (`.github/workflows/release.yml`) from a
  tagged commit, published as a GitHub Release with `SHA256SUMS`.
- **Pinned in the repo:** `plugins/agy/bin/SHA256SUMS` is committed through a
  reviewed PR after each release. The launcher refuses any binary whose
  SHA-256 doesn't match it — on download and on every run (so a tampered
  cache is caught too). Your trust root is this repository.
- The launcher's only network request is that one download, over https,
  with timeouts. Concurrent first runs are safe. (Between verifying and
  executing there is a short window in which a process running as *you*
  could swap the cached file — such a process could already do anything.)

## Limits we know about

- Default mode is agy's to define; see above.
- Hard links to secrets can't be distinguished from regular files by path.
- With `AGY_SCRAPE_USE_PROXY=1`, DNS rebinding protection is the proxy's job.
- Secret scanning is pattern-based and will miss unusual formats.
- `rescue` and `image` can do whatever agy can do with auto-approval; their
  rails limit the blast radius in your repo, not on your machine.

## Reporting a vulnerability

If you find a security issue in this plugin (not in `agy` itself, Claude
Code, or Google's services), report it privately:

- open a [private security advisory](https://github.com/limeflash/antigravity-plugin-cc/security/advisories/new), or
- contact the maintainer via GitHub: <https://github.com/limeflash>.

Please don't open a public issue. You'll get an acknowledgement within 7 days;
fixes aim to ship within 30 days depending on severity.

## Scope

**In scope:** shell or argument injection (including flag injection into
agy, git or curl) through slash-command arguments;
ways to make a read-only command write, run commands or reach your repo;
bypasses of the secret, SSRF, document-path or image-path guards; the job
state under `.agy-plugin/` (symlinks, predictable ids, races); the launcher's
download and verification; anything that makes the binary run code it
shouldn't.

**Out of scope:** vulnerabilities in `agy` (report to Google) or Claude Code
(report to Anthropic); attacks that already need shell access to your
machine; Google's OAuth flow and install script; best-practice suggestions
without a concrete exploit (open a regular issue).

## Supported versions

Only the latest release is supported.

## Hall of fame

Researchers who report valid issues are credited in
[CHANGELOG.md](./CHANGELOG.md) under the fix entry (unless they prefer to
stay anonymous).
