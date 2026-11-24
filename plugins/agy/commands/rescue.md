---
description: Delegate a coding task to agy with write access, job control and safety rails (isolate, clean-tree guard, post-run diff).
argument-hint: "[--isolate] [--allow-dirty] [--background] [--wait] [--resume|--fresh] [--model <alias>] <task>"
allowed-tools: Bash(bash:*)
---

Hand the user's task to the Antigravity CLI (`agy`) with permission to
edit files, plus background execution, job control, and persistent state
under `<repo>/.agy-plugin/`.

Use `/agy:rescue` instead of `/agy:delegate` when the user wants to:
- Let agy change code (delegate/ask are read-only).
- Start the work and come back later (`--background`).
- Block on a background task with a 10-minute deadline (`--background --wait`).
- Continue the most recent agy conversation (`--resume`). The default,
  `--fresh`, starts a new one.
- Get a job id they can later inspect with `/agy:status`, fetch with
  `/agy:result`, or stop with `/agy:cancel`.

For one-shot, "ask agy a question and read the reply" UX, prefer
`/agy:ask`.

## How to invoke

The user's request (treat as opaque text — pass it as a single
shell-safe argument; do **not** interpolate or splice it into the
command, do **not** strip characters like `"`, `$`, `;`, `\` or
backticks):

```
$ARGUMENTS
```

Lift any leading flags out of `$ARGUMENTS` and place them before the
task text:

- `--isolate`, `--allow-dirty`, `--background`, `--wait`, `--resume`,
  `--fresh`: boolean
- `--model <alias>` or `--model=<alias>`: value
- Other agy flags (e.g. `--print-timeout 20m`, `--effort high`) are
  forwarded to agy as-is. Only known agy value-flags consume the next
  word, so the task text is never swallowed.

Use the `Bash` tool to run:

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" rescue [flags...] '<task-text>'
```

…with `<task-text>` quoted as one shell argument so characters like
`"`, `$`, `;`, `\` and backticks cannot break out.

Examples (after you've parsed the flags out):

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" rescue 'investigate the flaky test in src/db/queries.test.ts'
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" rescue --isolate 'rewrite the cache layer to use Redis'
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" rescue --background 'audit the auth layer for missing input validation'
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" rescue --background --wait 'fix the off-by-one in pagination'
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" rescue --model opus --print-timeout 20m 'migrate the tests to vitest'
```

`<alias>`: `flash-low`, `flash-medium`, `flash`, `pro-low`, `pro`,
`sonnet`, `opus`, `gpt-oss` — resolved to the newest model of that
family that `agy models` lists and passed to agy's `--model` for this
run only. Any id or display name from `agy models` is also accepted.

## Background, foreground, and the job id

- **Foreground** (default): runs `agy` now and prints its reply (and
  the change summary) when the run ends. A job record is still kept.
- **Background** (`--background`): creates a job record, spawns a
  detached worker, and returns the job id (e.g. `agy-3a7b9c12`)
  immediately. The user can keep working and come back to it.
- **Background + wait** (`--background --wait`): starts in the
  background but blocks here until the job ends or 10 minutes pass.

Always surface the job id to the user in background mode so they can
pass it to `/agy:status`, `/agy:result`, or `/agy:cancel`.

## Safety: how rescue protects your repo

`/agy:rescue` is a *delegated coding task*, so agy runs with
`--dangerously-skip-permissions` and write access to the workspace —
it can create/edit/delete files without prompting. Three rails keep
that safe:

1. **Clean-tree guard.** In a git repo, rescue **refuses to run on a
   dirty working tree** (uncommitted changes), so you always have a
   clean baseline to revert to. Commit/stash first, use `--isolate`,
   or pass `--allow-dirty` to override.
2. **Post-run diff.** After a non-isolated rescue, it prints a
   `git diff --stat` + new-files summary, so you see exactly what agy
   changed before committing.
3. **`--isolate` (strongest).** agy edits a separate checkout
   (`git clone --shared` of HEAD, with its own `.git`), so its edits
   don't land in your working tree. It is not a sandbox — agy still runs
   with auto-approval. The result is saved as a patch under `.agy-plugin/patches/<job>.patch` with
   `git apply` / discard instructions. Foreground only (it shows you
   the patch). Requires a git repo.

**Recommend `--isolate` for anything non-trivial**, or for repos with
work you can't afford to lose.

## Operating rules

- `--isolate` overrides `--background` (it runs foreground to show you
  the patch).
- `--resume` and `--fresh` are mutually exclusive (exit 64).
- Non-isolated rescue in a non-git directory warns (no safety net) but
  proceeds.
- If it reports `agy is not installed` or `not authenticated`, tell the
  user to run `/agy:setup`.
- If it exits 64 with "task description is required", ask the user
  what they want agy to do.

Return its stdout verbatim — no extra commentary before or after. The
plugin's output is the canonical record of what happened.
