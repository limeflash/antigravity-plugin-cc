---
description: Print the captured output of an /agy:rescue or /agy:review job (latest by default).
argument-hint: "[job-id|prefix]"
allowed-tools: Bash(bash:*)
---

Fetch the captured output of an agy job (`/agy:rescue`, `/agy:review`,
`/agy:adversarial-review`). Defaults to the most recent job in this
workspace.

## How to invoke

```
$ARGUMENTS
```

If empty:

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" result
```

If the user passed an id or prefix (quote it as one shell argument):

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" result '<id-or-prefix>'
```

## Behavior notes

- It prints the job's detail header first (status, exit code, …), then a
  `--- output ---` separator, then the contents of the log file at
  `<repo>/.agy-plugin/logs/<id>.log`.
- For a job that's still running, the log is whatever the worker
  has flushed so far. There's no guarantee it ends on a clean line.
- An unknown or ambiguous id exits 2 (ambiguous prefixes list the
  candidates).

Return its stdout verbatim.
