---
name: antigravity-cli
description: Internal runtime contract for invoking the Antigravity CLI (`agy`) from the `agy:runner` subagent. Not user-invocable.
user-invocable: false
---

# Antigravity CLI runtime

Use this skill only inside the `agy:runner` subagent.

## Primary helper

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" ask [--model <alias>] '<prompt>' [--print-timeout <dur>] [--effort <level>]
```

`bin/agy-plugin` is a launcher for the plugin's native binary: on first use
it downloads the build for this machine and verifies its SHA-256 on every
run. The same line works on macOS, Linux and Windows (Git Bash). No Node.js
is needed.

`ask`:

- Locates `agy` and checks it is logged in (system keyring OAuth or
  `ANTIGRAVITY_API_KEY`) and is version 1.2.0 or newer.
- Runs agy non-interactively and returns the answer from agy's JSON output.
- Is **read-only**: agy runs from a throwaway directory (the repo is outside
  its workspace), pinned to its Default agent mode for that run, without
  `--dangerously-skip-permissions`. Writes are denied and reported as
  `agy tried to use X, which this read-only command blocks`.
- `--model <alias>` must come **before** the prompt. It is resolved at call
  time and passed to agy's own `--model` flag for that call only.
- After the prompt, only `--print-timeout` and `--effort` are accepted;
  any other agy flag is refused (exit 64).

## Rules of engagement

One `ask` call per task. The subagent is a forwarder, not an orchestrator —
keep the user's task text intact (one shell-quoted argument) and let `agy`
do the work.

Strip flags that belong to the parent slash command (`--background`) before
forwarding.

## Model aliases

- `gemini` — the newest Gemini of any kind (Gemini 4 Hydrogen today).
- Any Gemini kind that `agy models` lists — `hydrogen`, `flash`, `pro`, and
  whatever code name Google adds next — is an alias by itself.
- Add a level to either: `-xhigh`, `-high` (the default), `-medium`
  (`-med`), `-low` — e.g. `hydrogen-xhigh`, `flash-low`, `gemini-medium`.
- `opus` (`claude-opus`), `sonnet` (`claude-sonnet`), `gpt-oss`
  (`gpt-oss-120b`).

Each resolves against `agy models` to the newest model in its family. Any id or display name that
`agy models` lists is also accepted verbatim.

Run `bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" help` for the
authoritative list with the models currently available.

## Timeouts

`ask` gives agy 8 minutes by default (`AGY_ASK_TIMEOUT`, a Go duration like
`8m0s`). `--print-timeout` after the prompt overrides agy's limit, but the
plugin itself stops agy 2 minutes after `AGY_ASK_TIMEOUT`, so a longer run
needs that variable raised in the environment.

## What this skill does NOT do

- Does not install or authenticate `agy`. That is `/agy:setup`'s job.
- Does not edit files. Write-capable work is `/agy:rescue`.
- Does not retry, summarize, or post-process `agy`'s output.
- Does not read files, run `git`, or make HTTP calls outside the helper.

## Error handling

If the helper exits non-zero, return its stderr verbatim. Exit codes:

- `127` — `agy` is not installed.
- `1` — runtime failure: not authenticated, agy older than 1.2.0, or agy
  returned no answer.
- `64` — usage error: missing prompt, a refused flag, or an unknown
  `--model` name.
