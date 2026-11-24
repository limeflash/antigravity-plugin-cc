<div align="center">

# agy for Claude Code

**Use Google's [Antigravity CLI (`agy`)](https://antigravity.google/) without leaving Claude Code.**<br>
Ask questions, get a second opinion on your diff, hand off tasks, read web pages and documents — safely.

[![CI](https://github.com/limeflash/antigravity-plugin-cc/actions/workflows/ci.yml/badge.svg)](https://github.com/limeflash/antigravity-plugin-cc/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/limeflash/antigravity-plugin-cc?sort=semver&color=blue)](https://github.com/limeflash/antigravity-plugin-cc/releases)
[![Platforms](https://img.shields.io/badge/macOS%20·%20Linux%20·%20Windows-informational)](#-requirements)
[![Rust, zero dependencies](https://img.shields.io/badge/rust-zero%20dependencies-orange)](./CONTRIBUTING.md)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](./LICENSE)

</div>

```text
> /agy:review --base main
## Correctness
- calc.js:1 — `add` returns a - b instead of a + b (HIGH)
## Edge cases
- calc.js:2 — `divide` has no guard for b === 0 (MEDIUM)
```

---

## ✨ Why use it

| | |
|---|---|
| 🔒 **Read-only really means read-only** | Commands like `/agy:ask` and `/agy:review` can look, never touch: agy is explicitly forbidden to change files or run commands, and if that can't be guaranteed the plugin refuses to run. |
| 🛡️ **Nothing sensitive slips out** | Your diff is checked for API keys before it goes to Gemini; web addresses and file paths are checked so agy can't be pointed at your internal network or your `~/.ssh`. |
| 🎯 **Always the newest model** | `--model gemini` picks the newest Gemini (Gemini 4 Hydrogen today); `hydrogen-xhigh`, `flash`, `opus`, `sonnet` pick the latest of that family your account offers. |
| ⏳ **Long jobs in the background** | Start a review or a task, keep working, check back with `/agy:status` and `/agy:result`. |
| 📦 **Tiny and self-contained** | One ~0.6 MB native program with zero third-party dependencies. No Node.js, no Python. Verified by checksum before it runs. |

---

## 🚀 Quick start

**1. Install the plugin** (in Claude Code):

```text
/plugin marketplace add limeflash/antigravity-plugin-cc
/plugin install agy@limeflash-antigravity
/reload-plugins
```

**2. Check that everything is ready:**

```text
/agy:setup
```

It checks that `agy` is installed (and offers to install it), logged in, and new enough.

**3. Use it:**

```text
/agy:ask explain Rust lifetimes in one paragraph
```

> The first command downloads the plugin's binary for your computer (≈0.6 MB) from this repo's releases and verifies its checksum. After that it runs offline.

---

## 🧰 Commands

### Read-only — agy can read, but can't change anything

| Command | What it does |
|---|---|
| `/agy:ask <question>` | Ask anything, get agy's answer back. |
| `/agy:review [focus]` | Review your uncommitted changes. Add `--base main` to review a whole branch. |
| `/agy:adversarial-review [focus]` | A tougher review: questions the design and ends with *ship / change / rethink*. |
| `/agy:scrape <url>` | Turn a web page into clean Markdown. |
| `/agy:doc-to-md <file>` | Turn a PDF, DOCX, HTML, … into Markdown. |
| `/agy:delegate`, `/agy:research` | Hand an investigation to a subagent that asks agy for you. |

### Can change files — with safety rails

| Command | What it does |
|---|---|
| `/agy:rescue <task>` | Let agy fix or build something in your repo. It refuses if you have uncommitted changes and shows you what changed afterwards. Add **`--isolate`** to get a patch instead of edits. |
| `/agy:image <description>` | Generate an image; `--output path.png` copies it into your project. |

### Background jobs & help

| Command | What it does |
|---|---|
| `/agy:status` · `/agy:result` · `/agy:cancel` | Watch, read and stop background jobs. |
| `/agy:setup` · `/agy:help` | Check your setup · see all commands and the current models. |

**Useful flags:** `--model <name>` on most commands · `--background` (and `--wait`) on `review`, `adversarial-review` and `rescue`.

---

## 🎯 Choosing a model

```text
/agy:review --model gemini
/agy:ask --model hydrogen-xhigh "what's wrong with this regex?"
/agy:review --model flash-low
```

| Alias | Picks | Today¹ |
|---|---|---|
| `gemini` | newest Gemini of any kind | Gemini 4 Hydrogen (High) |
| `hydrogen` | newest Gemini Hydrogen | Gemini 4 Hydrogen (High) |
| `flash` | newest Gemini Flash | Gemini 3.8 Flash (High) |
| `pro`, `carbon`, … | **any** Gemini kind your account lists | — |
| `opus`, `sonnet` | newest Claude Opus / Sonnet (prefers *Thinking*) | Claude Opus 4.6 / Sonnet 4.6 |
| `gpt-oss` | GPT-OSS 120B | GPT-OSS 120B (Medium) |

**Levels:** add `-xhigh`, `-high` (default), `-medium` or `-low` to any Gemini alias — `hydrogen-xhigh`, `flash-low`, `gemini-medium`. When Google ships a new Gemini code name, its alias works right away, no plugin update.

¹ What `agy models` listed on 2026-11-25; your account may differ. Any exact model name or id from `agy models` works too. `/agy:help` shows what each alias points to right now. Without `--model`, agy uses its own default.

---

## 🔒 How the safety works

**Read-only commands** run agy:

- 📁 **in a throwaway folder** — your repository is never agy's workspace;
- 🔐 **locked down** — agy is told *Default mode, no shell commands, no file writes*, which also stops it from reading files outside that folder;
- 🚫 **fail-safe** — if agy can't be locked down (some versions can't), the command refuses to run;
- 👀 **transparently** — if agy tries something it isn't allowed to, you see a note saying exactly what was blocked.

This matters because since agy **1.5.7**, its default mode lets it edit files in any git repository without asking. Older versions of this plugin (≤ 0.8.1) were affected; see [SECURITY.md](./SECURITY.md).

**Before anything is sent to Gemini:**

| | |
|---|---|
| `/agy:review` | Scans the diff *and* the changed files for AWS, GitHub, Slack, OpenAI, Anthropic keys, private keys and `password=` lines. Stops with a warning if it finds any. |
| `/agy:scrape` | Only `http`/`https`. Blocks localhost, private networks and cloud-metadata addresses, including disguised ones and DNS tricks. The plugin downloads the page itself; agy only reads the copy. |
| `/agy:doc-to-md` | Only document types (pdf, docx, html, md, …) and never from `~/.ssh`, `~/.aws`, keychains, browser profiles, `/etc`, …. |

**Commands that change files** (`/agy:rescue`, `/agy:image`) do give agy permission to act. Their rails: a clean git tree first, a summary of every change afterwards, and `--isolate` to work on a separate copy.

---

## ⚙️ How it works

```
 Claude Code
     │  /agy:review
     ▼
 bin/agy-plugin ──(first run)──▶ downloads the binary for your OS from GitHub Releases
     │                            and runs it only if its SHA-256 matches this repo
     ▼
 agy-plugin  (Rust, zero dependencies)
     │  stages the diff · checks for secrets · locks agy down
     ▼
 agy  (your install, your login, your settings)
```

The plugin never replaces agy: it drives the `agy` you already have, with your account and your `AGENTS.md` / `GEMINI.md`.

---

## 📋 Requirements

- **Claude Code** with plugin support
- **Antigravity CLI (`agy`) 1.2.0+**, logged in (run `agy` once, or set `ANTIGRAVITY_API_KEY`). `/agy:setup` can install it.
- **git** for review and rescue
- **macOS** (Apple silicon / Intel), **Linux** (x86_64 / arm64), **Windows** (x86_64 / arm64, via the Git Bash Claude Code uses)

---

<details>
<summary><b>🔧 Configuration (environment variables)</b></summary>

| Variable | Default | Effect |
|---|---|---|
| `AGY_ASK_TIMEOUT` · `AGY_REVIEW_TIMEOUT` · `AGY_SCRAPE_TIMEOUT` · `AGY_DOCTOMD_TIMEOUT` · `AGY_IMAGE_TIMEOUT` | 8m · 10m · 5m · 8m · 8m | how long agy may take |
| `AGY_RESCUE_TIMEOUT` | none | time limit for rescue |
| `AGY_REVIEW_CONTEXT` | 25 | lines of context around each change |
| `AGY_REVIEW_ALLOW_SECRETS` | — | `1` = send even if keys were found (warns) |
| `AGY_SCRAPE_USE_PROXY` | — | `1` = let scrape use your HTTP proxy |
| `AGY_BIN` | from `PATH` | path to agy |
| `AGY_PLUGIN_CACHE` | `~/.cache/agy-plugin` | where the binary is kept |
| `AGY_PLUGIN_BIN` | — | use a locally built binary (development) |

</details>

<details>
<summary><b>❓ FAQ</b></summary>

**Do I need an Antigravity subscription?**
You need an account `agy` accepts: Google AI Pro / Ultra, Code Assist Standard / Enterprise, or an enterprise GCP project. See the [Antigravity docs](https://antigravity.google/docs/cli-overview).

**Does the plugin send my data anywhere?**
Only through agy, the way agy normally does. The plugin's own single network request is downloading its binary from this repository's releases.

**Will it change my agy settings?**
No. `--model` is passed per call; your `settings.json` is never edited.

**Why does `rescue` need full permissions?**
Because it edits files and a background agy can't stop to ask. That's why it insists on a clean git tree, shows the changes, and offers `--isolate`.

**Can I still use agy normally?**
Yes. The plugin uses your existing install and leaves it as it is.

</details>

<details>
<summary><b>🛠️ Development</b></summary>

```bash
cargo test                                  # unit + end-to-end tests (against a fake agy)
cargo clippy --all-targets -- -D warnings
sh scripts/test-launcher.sh                 # launcher download & checksum tests
scripts/e2e-live.sh                         # against a real, logged-in agy (uses quota)
export AGY_PLUGIN_BIN=$PWD/target/release/agy-plugin   # try your build in Claude Code
```

Rules of the house — including *std only, zero crates* — are in [CONTRIBUTING.md](./CONTRIBUTING.md); releases in [RELEASING.md](./RELEASING.md).

</details>

---

## 🙏 Credits

Started from [@simplybychris](https://github.com/simplybychris)'s [`antigravity-plugin-cc`](https://github.com/simplybychris/antigravity-plugin-cc) (MIT); the model aliases follow its 0.5.0. Background-job ideas are inspired by [`openai/codex-plugin-cc`](https://github.com/openai/codex-plugin-cc) (Apache-2.0). No code is copied from either; see [NOTICE](./NOTICE).

**License:** [MIT](./LICENSE) · **Changes:** [CHANGELOG.md](./CHANGELOG.md) · **Security:** [SECURITY.md](./SECURITY.md)
