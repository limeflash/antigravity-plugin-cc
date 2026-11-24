---
description: Verify the Antigravity CLI (`agy`) is installed, authenticated and new enough; offer to install it if missing
allowed-tools: Bash(bash:*), Bash(curl:*), AskUserQuestion
---

Run:

```bash
bash "${CLAUDE_PLUGIN_ROOT}/bin/agy-plugin" check
```

(On first use the launcher downloads the plugin binary for this machine and
verifies its SHA-256; that note on stderr is expected. On Windows the same
line works through Git Bash.)

Then interpret the JSON output
(`installed`, `path`, `version`, `auth`, `pluginVersion`, `supported`, `error`):

- If `installed: false`, use `AskUserQuestion` once to offer installation.
  Options:
  - `Install agy now (Recommended)` — run the official installer:
    ```bash
    curl -fsSL https://antigravity.google/cli/install.sh | bash
    ```
    Then re-run the check.
  - `Skip for now` — explain that the `/agy:*` commands (`/agy:ask`,
    `/agy:review`, `/agy:rescue`, …) will fail until `agy` is installed.

- If `installed: true` and `supported: false` with a non-empty `error`, the
  installed agy is older than 1.2.0. Tell the user to run `agy update`,
  then re-run `/agy:setup`.

- If `version` is `unknown` (then `supported` is also false, with an empty
  `error`), agy's version couldn't be read. Report that and suggest
  `agy update`; commands will refuse to run if agy turns out to be too old.

- If `installed: true` but `auth: missing`, tell the user to either:
  - run `!agy` once interactively to complete OAuth (cached in the system
    keyring), **or**
  - export `ANTIGRAVITY_API_KEY` in their shell rc and reload it.

- If `installed: true`, `supported: true`, and `auth` is `api-key` or
  `oauth`, report that everything is ready — one short status line with the
  agy version and plugin version is enough.

- If the command prints no JSON (for example the launcher could not
  download the binary or its checksum did not match), show that error
  verbatim; do not try to work around the checksum check.
