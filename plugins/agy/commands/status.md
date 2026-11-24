---
description: Show the recent agy jobs (/agy:rescue, /agy:review) in this workspace, or a single job's details.
argument-hint: "[job-id|prefix]"
allowed-tools: Bash(bash:*)
---

List recent agy jobs (no argument) or show the detail block for a
single job (when an id or prefix is provided). Jobs come from
`/agy:rescue`, `/agy:review` and `/agy:adversarial-review`, foreground
or background.

## How to invoke

The user's request:

```
$ARGUMENTS
```

If `$ARGUMENTS` is empty, run:

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" status
```

If `$ARGUMENTS` contains a job id (full `agy-xxxxxxxx`) or a unique
prefix (`agy-3a7b`), pass it as one quoted argument:

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" status '<id-or-prefix>'
```

## Behavior notes

- Jobs are stored per workspace under `<repo>/.agy-plugin/jobs/`, and the
  list shows the 10 most recent. Running `/agy:status` from another
  project will not show them — that's intentional.
- A `running` record whose worker PID is dead is shown as `failed`
  (without rewriting the record), with a note in the detail block.
- An unknown id exits 2; an ambiguous prefix exits 2 and lists the
  candidates so the user can pick a longer prefix.

Return its stdout verbatim.
