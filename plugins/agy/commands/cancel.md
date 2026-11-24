---
description: Cancel a running background /agy:rescue or /agy:review job (stops the worker and agy itself).
argument-hint: "[job-id|prefix]"
allowed-tools: Bash(bash:*)
---

Stop an active background job started by `/agy:rescue --background`,
`/agy:review --background` or `/agy:adversarial-review --background`.
Defaults to the latest job in this workspace.

## How to invoke

```
$ARGUMENTS
```

If empty:

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" cancel
```

If the user passed an id or prefix (quote it as one shell argument):

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" cancel '<id-or-prefix>'
```

## Behavior notes

- Cancel stops the worker's whole process group — the worker **and** the
  `agy` process it started: `SIGTERM` first, then `SIGKILL` after a
  5-second grace. On Windows it uses `taskkill /T` on the process tree.
- Only jobs you started on this machine can be stopped: cancel finds the
  worker through a private per-user registry and signals it only while the
  worker provably holds its lock. A job whose worker is already gone is
  marked `canceled` without sending any signal; a record that came with
  the repository (`not-started-here`) is marked canceled and nothing is
  stopped or deleted.
- With no id, it cancels the newest job that is still running or pending.
- A completed/failed/already-canceled job exits 2 saying it is already
  finished — there's nothing to cancel. An unknown or ambiguous id also
  exits 2.
- A canceled `/agy:rescue` may have edited files already; suggest
  `git status` / `git diff` to see what it left behind.

Return its stdout verbatim.
