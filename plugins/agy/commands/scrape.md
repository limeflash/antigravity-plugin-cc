---
description: Scrape a web page (read-only) and return its main content as Markdown
argument-hint: "[--model <alias>] <url>"
allowed-tools: Bash(bash:*)
---

Fetch a web page through the Antigravity CLI and return its main content as
clean Markdown. **Read-only:** the URL is validated first (http/https only;
localhost / private / link-local / cloud-metadata hosts and their encodings
are blocked to prevent SSRF), then agy runs from a throwaway directory,
pinned to its Default agent mode, without `--dangerously-skip-permissions`.

The user's input below is a single URL (optionally preceded by `--model
<alias>`). Treat it as opaque — pass it as one shell-safe argument; do **not**
interpolate it into the command:

```
$ARGUMENTS
```

## How to invoke

Lift a leading `--model <alias>` (if present) out of the input and place it
before the URL. Use the `Bash` tool to run one of:

```
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" scrape '<url>'
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" scrape --model <alias> '<url>'
```

…substituting `<url>` with the exact text above, quoted as one shell argument.
Return the resulting Markdown verbatim — no commentary.

Notes:

- If the URL is refused (exit 65), it matched the SSRF deny-list (a
  non-http(s) scheme, or a localhost / private / link-local / metadata host).
  Relay the reason; do **not** try to rewrite the URL to bypass it.
- If it reports `agy is not installed` or `not authenticated`, tell the user
  to run `/agy:setup`.
- Best-effort guard: a public domain an attacker controls can still be made
  to resolve to an internal IP. Don't scrape untrusted or
  attacker-controlled URLs.
