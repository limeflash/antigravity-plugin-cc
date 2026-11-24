---
description: Ask Antigravity to review your git diff (read-only). Add --base <ref> for branch review or --background to run later.
argument-hint: "[--base <ref>] [--background] [--wait] [--model <alias>] [focus text]"
allowed-tools: Bash(bash:*)
---

Run a read-only code review of your changes through `agy`. By default it
reviews the working-tree diff against `HEAD`; `--base <ref>` reviews the
branch instead.

## How to invoke

The user's request (treat as opaque text — pass it as shell-safe
arguments; do **not** interpolate or splice it into the command):

```
$ARGUMENTS
```

Lift any flags out of `$ARGUMENTS` and place them before the focus text.
Quote the remaining focus text as **one** shell argument so characters
like `"`, `$`, `;`, `\` and backticks cannot break out; omit it if there
is none. Use the `Bash` tool to run:

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" review [flags...] ['<focus-text-here>']
```

Flags:

- `--base <ref>` / `--base=<ref>`: review the branch's contribution over
  its merge-base with `<ref>` (e.g. `--base main`).
- `--background`: return a job id immediately and run agy in a detached
  worker. Check progress with `/agy:status`, read the output with
  `/agy:result`, stop it with `/agy:cancel`.
- `--wait`: with `--background`, block until the job ends or the
  10-minute deadline.
- `--model <alias|name>`: per-call model selection (see `/agy:help`).

Examples (after parsing flags out of `$ARGUMENTS`):

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" review
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" review 'focus on error handling'
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" review --base main 'focus on the new SQL'
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" review --base main --background --wait
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" review --model opus 'focus on concurrency'
```

## Behavior notes

- **Branch diff** is computed against the merge-base of HEAD and the
  base ref, so unrelated changes that landed on the base since you
  branched are excluded.
- If there is no diff (clean working tree, or empty branch diff), it
  exits 1 with a clear message.
- **Secret guard.** If the diff (or the full content of a changed file)
  matches common secret patterns, it refuses with exit 65 and lists
  them. Relay that; the user can remove the values or set
  `AGY_REVIEW_ALLOW_SECRETS=1` themselves.
- **Read-only by construction.** agy runs from a throwaway directory
  holding the staged diff and file copies (your repo is outside its
  workspace), pinned to its Default agent mode, without
  `--dangerously-skip-permissions`. Writes are denied and reported as
  a note (`agy tried to use X …, which this read-only command blocks`). Background
  runs complete unattended — there's no write prompt to stall on.
- If it reports `agy is not installed` or `not authenticated`, tell the
  user to run `/agy:setup`.

Return whatever it prints, verbatim. No commentary before or after.
