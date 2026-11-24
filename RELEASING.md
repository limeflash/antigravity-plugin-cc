# Releasing

The plugin's code (slash commands, launcher) ships from `main`; the binary
ships from GitHub Releases, pinned by hash in `plugins/agy/bin/`.

1. **Prepare** on a branch: bump the version in `Cargo.toml`,
   `plugins/agy/.claude-plugin/plugin.json` and `.claude-plugin/marketplace.json`
   (both fields), add the `CHANGELOG.md` section, run `cargo test`,
   `sh scripts/test-launcher.sh` and `scripts/e2e-live.sh`. Merge.
2. **Tag** the merge commit: `git tag -a vX.Y.Z -m "vX.Y.Z" && git push origin vX.Y.Z`.
   The tag must equal the `Cargo.toml` version (the workflow checks).
3. **Release workflow** (`.github/workflows/release.yml`) builds all six
   targets, publishes the GitHub Release with the binaries and
   `SHA256SUMS`, and opens a PR `release: pin agy-plugin binaries to vX.Y.Z`
   that updates `plugins/agy/bin/VERSION` and `SHA256SUMS`.
4. **Review the pin PR**: compare its `SHA256SUMS` with the release asset,
   then merge. Users get the new binary from that moment — the launcher
   refuses anything whose hash isn't committed.

Until the pin PR is merged, users keep the previous binary, which is fine:
the launcher pins a binary version independently of the plugin files.
