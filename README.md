# agy — the Antigravity CLI plugin for Claude Code

[![CI](https://github.com/limeflash/antigravity-plugin-cc/actions/workflows/ci.yml/badge.svg)](https://github.com/limeflash/antigravity-plugin-cc/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/limeflash/antigravity-plugin-cc?sort=semver&color=blue)](https://github.com/limeflash/antigravity-plugin-cc/releases)
[![Platforms](https://img.shields.io/badge/platforms-macOS%20·%20Linux%20·%20Windows%20·%20WSL-informational)](#requirements)
[![Rust: std only](https://img.shields.io/badge/rust-std%20only%2C%20zero%20crates-orange)](#how-it-works)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](./LICENSE)

**Call Google's [Antigravity CLI (`agy`)](https://antigravity.google/) from inside Claude Code** — ask
questions, review your diff, delegate tasks, scrape the web, convert
documents to Markdown — without leaving your editor.

What sets it apart: the read-only commands come with a **real read-only
guarantee** — `agy` runs outside your repository and is pinned to a mode in
which it *cannot write* — and everything that leaves your machine is guarded
(credential scan before anything reaches Gemini, SSRF filter on URLs,
sensitive-path filter on files). One small native binary, **written in Rust
with the standard library only — zero third-party crates**.

> **Latest — v0.9.0: rewritten in Rust.** One binary replaces the Bash
> wrapper, the Node.js companion and the PowerShell port — Node.js is no
> longer needed. Closes a **read-only escape on agy 1.5.7+** (see
> [SECURITY.md](./SECURITY.md)), passes `--model` natively, and fixes a dozen
> job-control bugs. Validated end-to-end against `agy` 1.5.16 on macOS.

## Why this plugin

- 🔒 **Read-only means read-only.** `/agy:ask`, `/agy:review`,
  `/agy:adversarial-review`, `/agy:scrape` and `/agy:doc-to-md` run `agy`
  from a throwaway directory (your repo is outside its workspace), pinned to
  agy's **Default agent mode** for that run, without
  `--dangerously-skip-permissions`, with explicit deny rules for every shell
  command and file write. Writes are denied and reported. Since
  agy 1.5.7, headless runs otherwise start in *Auto* mode, which approves
  writes outside the workspace — we verified agy overwriting a file in an
  unrelated git repo that way, and pin it shut.
  ([how it works](#how-it-works) · [SECURITY.md](./SECURITY.md))
- 🛡️ **Guards what leaves your machine.** Diffs *and the full content of
  changed files* are scanned for credentials before being sent to Gemini;
  scrape URLs are SSRF-filtered (localhost, private, link-local, cloud
  metadata, every numeric/IPv6/`nip.io` spelling of them); document paths
  are extension-allow-listed and refused under `~/.ssh`, `~/.aws`,
  `~/.gemini`, `/etc` and friends.
- 🧰 **13 commands, one plugin.** ask · delegate · research · review ·
  adversarial-review · rescue · scrape · doc-to-md · image · status · result
  · cancel · setup.
- ⚙️ **Background job control.** Long reviews and rescues run detached with
  real job records (`/agy:status`, `/agy:result`, `/agy:cancel` — cancel
  stops the worker *and* agy); `rescue --isolate` works in a separate
  checkout and hands you a reviewable patch.
- 🎯 **Models that stay current.** `--model flash` / `pro` / `opus` /
  `sonnet` resolve at call time against `agy models` to the newest model in
  the family and go to agy's own `--model` flag. No plugin update when
  Google ships a model; your agy settings are never touched.
- 📦 **Verified binary.** The launcher downloads the binary for your
  platform from this repo's GitHub Release once, and checks its SHA-256
  against the hash committed here on every run.

## What you get

**Read-only** (agy can read, never change anything):

- **`/agy:ask [--model <alias>] <prompt>`** — one-shot prompt; returns agy's
  answer verbatim.
- **`/agy:review [--base <ref>] [--background] [--wait] [--model <alias>] [focus]`**
  — review the working-tree diff, or the branch since its merge-base with
  `<ref>`. agy gets the expanded diff (`-U25`) plus the full current content
  of the changed files, staged on disk.
- **`/agy:adversarial-review [same flags] [focus]`** — challenge-mode review:
  questions the design, proposes an alternative, ends with a
  ship / change / rethink verdict.
- **`/agy:scrape [--model <alias>] <url>`** — fetch a web page and return its
  main content as Markdown (SSRF-guarded).
- **`/agy:doc-to-md [--model <alias>] <path>`** — convert a local document
  (PDF, DOCX, HTML, …) to Markdown; agy sees one copy of that one file.
- **`/agy:delegate`, `/agy:research [--background] [--model <alias>]`** — hand
  an investigation to the `agy:runner` subagent (built on `ask`).

**Write-capable** (agy runs with auto-approval, behind rails):

- **`/agy:rescue [--isolate] [--allow-dirty] [--background] [--wait] [--resume|--fresh] [--model <alias>] <task>`**
  — let agy edit files. Refuses a dirty git tree (override with
  `--allow-dirty`) and prints a change summary afterwards; **`--isolate`**
  works in a separate checkout and saves a patch under
  `.agy-plugin/patches/` instead, so its edits don't land in your tree (it is
  not a sandbox — agy still runs with auto-approval).
- **`/agy:image [--name <slug>] [--output <path>] <description>`** — generate
  an image with agy's built-in `generate_image` tool.

**Jobs and setup:** `/agy:status [id]`, `/agy:result [id]`,
`/agy:cancel [id]`, `/agy:setup`, `/agy:help`.

## Requirements

- **Claude Code** with plugin-marketplace support.
- **Antigravity CLI (`agy`) 1.2.0 or newer**, logged in (OAuth after one
  interactive `agy` run, or `ANTIGRAVITY_API_KEY`). `/agy:setup` checks all
  of this and can install agy for you.
- **git** (for review and rescue) and a POSIX shell — on Windows, the Git
  Bash that Claude Code already uses. No Node.js, no Python.
- Platforms with prebuilt binaries: macOS (Apple silicon, Intel), Linux
  (x86_64, arm64; static musl builds), Windows (x86_64, arm64).

## Install

In Claude Code:

```text
/plugin marketplace add limeflash/antigravity-plugin-cc
/plugin install agy@limeflash-antigravity
/reload-plugins
/agy:setup
```

The first `/agy:*` command downloads the plugin's binary for your platform
(about 0.6 MB) from this repository's GitHub Releases and verifies it. If agy
is missing, `/agy:setup` offers the official installer:

```bash
curl -fsSL https://antigravity.google/cli/install.sh | bash
```

## Usage

```text
/agy:ask explain the difference between Go channels and Rust async in one paragraph
/agy:review
/agy:review --base main focus on error handling and concurrency safety
/agy:adversarial-review --background --model opus
/agy:rescue --isolate fix the off-by-one in pagination
/agy:scrape https://example.com/changelog
/agy:doc-to-md ./docs/spec.pdf
/agy:image --name hero --output ./assets/hero.png isometric illustration of a developer at a desk
```

You can also just ask Claude ("ask agy to look at this file and suggest a
simpler design") — the `agy:runner` subagent picks it up.

### Pick a model

```text
/agy:ask --model opus "explain Go's escape analysis"
/agy:review --model flash
```

Aliases: `flash-low`, `flash-medium`, `flash`, `pro-low`, `pro`, `sonnet`,
`opus`, `gpt-oss` — each resolves to the newest matching model that
`agy models` lists for your account. Any id or display name from
`agy models` works verbatim. `/agy:help` shows the live table. Without
`--model`, agy uses its own default.

## How it works

```
Claude Code → /agy:* → bin/agy-plugin (launcher) → agy-plugin (Rust) → agy --print …
```

- **The launcher** (`plugins/agy/bin/agy-plugin`, POSIX sh; a PowerShell
  twin for Windows without Git Bash) picks the binary for your OS/CPU,
  downloads it once from the release named in `bin/VERSION`, and refuses to
  run it unless its SHA-256 matches `bin/SHA256SUMS` — on every run, so a
  tampered cache is caught too.
- **The binary** is std-only Rust (no crates at all; OS calls via `extern`),
  so there is no dependency tree to audit or to be compromised through.
- **It drives your local agy** — your install, your login, your config
  (`AGENTS.md`, `GEMINI.md`, `settings.json` are read by agy as usual).

**Read-only commands.** agy runs with its cwd *and* only `--add-dir` set to a
fresh private temp dir (or, for reviews, a stage dir holding the diff and
copies of the changed files), with repo-location variables removed from its
environment, under `--sandbox`, and with one session-only setting as the last
token: `--settings={"agentMode":"","permissions":{"deny":["command(*)","write_file(*)"]}}`.
That selects agy's Default agent mode — a headless run *denies* anything
needing approval, including reading outside its workspace — and adds deny
rules for all shell commands and file writes that agy enforces in any mode.
agy reports blocked attempts as `denied_actions`, and the plugin shows them. If agy defaults to Auto mode
but can't be pinned, the plugin refuses to run rather than run unpinned.
Answers come from agy's `--output-format json`.

**Write-capable commands.** `rescue` and `image` run agy with
`--dangerously-skip-permissions` by design. `rescue` is guarded by the
clean-tree check, the post-run change summary and `--isolate`; `image` only
copies files that are inside agy's own artifact directories.

See [SECURITY.md](./SECURITY.md) for the full model and its limits.

## Configuration

| Variable | Default | Effect |
|---|---|---|
| `AGY_ASK_TIMEOUT`, `AGY_SCRAPE_TIMEOUT`, `AGY_DOCTOMD_TIMEOUT`, `AGY_REVIEW_TIMEOUT`, `AGY_IMAGE_TIMEOUT` | 8m0s, 5m0s, 8m0s, 10m0s, 8m0s | agy `--print-timeout` per command |
| `AGY_RESCUE_TIMEOUT` | none | `--print-timeout` for rescue |
| `AGY_REVIEW_CONTEXT` | 25 | `git diff -U<n>` for reviews |
| `AGY_REVIEW_ALLOW_SECRETS` | unset | `1` sends a diff that matches secret patterns anyway (with a warning) |
| `AGY_PROMPT_MAX_BYTES` | 30000 | prompt cap (Windows command-line limit) |
| `AGY_BIN` | PATH lookup | path to agy |
| `AGY_PLUGIN_BIN` | unset | run a locally built agy-plugin instead of the downloaded one |
| `AGY_PLUGIN_CACHE` | `~/.cache/agy-plugin` | where the launcher keeps the binary |

## FAQ

### Do I need an Antigravity subscription?

You need whatever account `agy` accepts: Google AI Pro, Ultra, Code Assist
Standard/Enterprise, or an enterprise GCP project. See the
[Antigravity docs](https://antigravity.google/docs/cli-overview).

### Does this plugin send data anywhere other than what agy sends?

The only network request the plugin itself makes is the one-time download of
its own binary from this repository's GitHub Releases (verified against the
committed SHA-256). Everything else goes through agy's normal channels.

### Can I keep using Antigravity outside this plugin?

Yes — the plugin uses your local install and never changes agy's settings.

### Why does `rescue` need auto-approval?

Because it edits files: a headless agy run can't stop to ask. That's why it
refuses a dirty tree, prints what changed, and offers `--isolate`.

## Development

```bash
cargo test                         # unit + end-to-end tests (stub agy)
cargo clippy --all-targets -- -D warnings
sh scripts/test-launcher.sh        # launcher download/verify tests
scripts/e2e-live.sh                # against a real, logged-in agy (uses quota)
AGY_PLUGIN_BIN=target/release/agy-plugin   # try a local build through the plugin
```

See [CONTRIBUTING.md](./CONTRIBUTING.md) — including the std-only rule — and
[RELEASING.md](./RELEASING.md).

## Credits

Originally seeded from [@simplybychris](https://github.com/simplybychris)'s
[`antigravity-plugin-cc`](https://github.com/simplybychris/antigravity-plugin-cc)
(MIT); the dynamic `--model` resolution follows its 0.5.0. Job-control
patterns are inspired by
[`openai/codex-plugin-cc`](https://github.com/openai/codex-plugin-cc)
(Apache-2.0). No source code is copied from either; see [NOTICE](./NOTICE).

## License

[MIT](./LICENSE). See [NOTICE](./NOTICE) for attribution.
