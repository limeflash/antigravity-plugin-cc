# Contributing

Thanks for considering a contribution! Design goals, in priority order:

1. **Safe by default.** Read-only commands stay read-only (Default agent
   mode pinned, no `--dangerously-skip-permissions`, never the repo as agy's
   workspace), and anything that leaves the machine is guarded.
2. **std-only Rust.** `[dependencies]` stays empty — no crates in any form,
   no vendored third-party code, no `build.rs`. If std doesn't wrap an OS
   call, declare it with `extern` in `src/sys.rs`. CI fails on any
   dependency. This keeps the binary small and the supply chain auditable.
3. **Cross-platform, equally.** macOS, Linux and Windows (Git Bash and
   native PowerShell). Use `std::path`, gate platform code with `cfg`, and
   check `cargo clippy --target x86_64-pc-windows-msvc` when you touch it.
4. **Useful to the typical Claude Code user calling `agy`.**

## Development

```bash
git clone git@github.com:<you>/antigravity-plugin-cc.git && cd antigravity-plugin-cc
cargo test                                  # unit + end-to-end tests against a stub agy
cargo clippy --all-targets -- -D warnings
cargo fmt
sh scripts/test-launcher.sh                 # launcher download/verify tests
```

The end-to-end tests in `tests/cli.rs` run the real binary against
`examples/stub_agy.rs`, which records how it was called; add a case there
for any change to how agy is invoked. `scripts/e2e-live.sh` runs the
commands against a real, logged-in agy (it uses model quota) — including the
read-only write-escape check; run it before a release.

To use your build from Claude Code, point the plugin at it:
`export AGY_PLUGIN_BIN=$PWD/target/release/agy-plugin`, then add your clone
as a local marketplace.

## Layout

- `src/` — the binary: `commands.rs` (the /agy:* commands), `capture.rs`
  (the read-only agy run), `agy.rs` / `models.rs` (probing agy, `--model`),
  `guard.rs` / `secrets.rs` / `fetch.rs` (input guards), `git.rs`,
  `jobs.rs` (job state and workers), `sys.rs` (OS calls), `json.rs`,
  `cli.rs`, `render.rs`, `prompts.rs`, `time.rs`, `util.rs`.
- `plugins/agy/` — what Claude Code installs: slash commands, the runner
  subagent, the skill, and `bin/` (launcher, `VERSION`, `SHA256SUMS`).

## What this project wants help with

- Better secret patterns (more true positives, no new false positives —
  bring sample diffs).
- `/agy:doc-to-md` for binary formats via a safe local extraction step.
- Real-world reports from running the commands on your own projects.

## What this project will say no to

- Any crate dependency.
- Weakening a guard by default (the secret scan's opt-out is
  `AGY_REVIEW_ALLOW_SECRETS=1` for a reason).
- Auto-approval (`--dangerously-skip-permissions`) on anything but rescue and
  image.

## Filing issues

Use the issue templates. For bugs include the output of `/agy:setup`, the
exact command, `agy --version`, and your OS. Security issues: see
[SECURITY.md](./SECURITY.md), never a public issue.

## Commit style

Conventional commits (`feat:`, `fix:`, `chore:`, `ci:`, `docs:`, `test:`).
Squash-merge by default.

## License

By contributing you agree your contributions are licensed under the
[MIT License](LICENSE).
