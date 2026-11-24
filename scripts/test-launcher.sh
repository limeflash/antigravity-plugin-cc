#!/bin/sh
# Tests for plugins/agy/bin/agy-plugin (the download-and-verify launcher).
# Uses a fake release served from file:// and a fake "binary": a shell
# script, or — where a script can't stand in for an executable (Windows Git
# Bash runs the .exe through Windows) — the stub given in AGY_TEST_FAKE_EXE
# (a built examples/stub_agy), which records its argv.
set -eu
repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'; else shasum -a 256 "$1" | awk '{print $1}'; fi
}

# Same platform detection as the launcher.
case "$(uname -s)" in Darwin) os=apple-darwin ;; Linux) os=unknown-linux-musl ;; *) os=pc-windows-msvc ;; esac
case "$(uname -m)" in x86_64 | amd64) arch=x86_64 ;; *) arch=aarch64 ;; esac
exe=""; [ "$os" = pc-windows-msvc ] && exe=.exe
asset="agy-plugin-$arch-$os$exe"

# A plugin copy whose VERSION/SHA256SUMS we control.
mkdir -p "$work/plugin/bin" "$work/release/v9.9.9"
cp "$repo/plugins/agy/bin/agy-plugin" "$work/plugin/bin/"
echo 9.9.9 >"$work/plugin/bin/VERSION"
fake="$work/release/v9.9.9/$asset"
if [ -n "${AGY_TEST_FAKE_EXE:-}" ]; then
  cp "$AGY_TEST_FAKE_EXE" "$fake"
  export STUB_RECORD="$work/record.txt" STUB_RESPONSE="fake-binary"
else
  printf '#!/bin/sh\necho "fake-binary args:$*"\n' >"$fake"
fi
# What the fake printed for "$@": the script echoes its args; the stub
# prints "fake-binary" and records argv (cwd TAB argv joined by 0x1f).
ran_with() {
  if [ -n "${AGY_TEST_FAKE_EXE:-}" ]; then
    printf 'fake-binary args:%s' "$(tail -n 1 "$STUB_RECORD" | cut -f2 | tr '\037' ' ')"
  else
    printf '%s' "$1"
  fi
}
good=$(sha256 "$work/release/v9.9.9/$asset")
echo "$good  $asset" >"$work/plugin/bin/SHA256SUMS"
launcher="$work/plugin/bin/agy-plugin"
# curl on Windows takes C:/... paths, not Git Bash's /tmp/... ones.
release_dir="$work/release"
command -v cygpath >/dev/null 2>&1 && release_dir="/$(cygpath -m "$release_dir")"
export AGY_PLUGIN_DOWNLOAD_BASE="file://$release_dir" AGY_PLUGIN_CACHE="$work/cache"

# 1. First run downloads, verifies, runs and passes args through.
out=$("$launcher" ask "two words" 2>/dev/null) || fail "first run failed"
out=$(ran_with "$out")
[ "$out" = "fake-binary args:ask two words" ] || fail "unexpected output: $out"
[ -x "$work/cache/9.9.9/$asset" ] || fail "binary not cached"
echo "ok 1 download + verify + exec"

# 2. Cached run works offline.
AGY_PLUGIN_DOWNLOAD_BASE="file://$release_dir/../nowhere" "$launcher" x >/dev/null 2>&1 || fail "cached run needed the network"
echo "ok 2 cached run"

# 3. A tampered cache entry is NOT executed; the launcher re-downloads.
printf '#!/bin/sh\necho PWNED\n' >"$work/cache/9.9.9/$asset"
out=$("$launcher" y 2>/dev/null) || fail "re-download failed"
out=$(ran_with "$out")
[ "$out" = "fake-binary args:y" ] || fail "tampered cache was executed: $out"
echo "ok 3 tampered cache refused"

# 4. A tampered release asset is refused (and not left in the cache).
rm -rf "$work/cache"
printf '#!/bin/sh\necho PWNED\n' >"$work/release/v9.9.9/$asset"
if out=$("$launcher" z 2>&1); then fail "tampered asset was accepted: $out"; fi
case "$out" in *"checksum mismatch"*) ;; *) fail "no mismatch message: $out" ;; esac
[ ! -e "$work/cache/9.9.9/$asset" ] || fail "tampered asset left in cache"
for f in "$work/cache/9.9.9/"*download*; do [ -e "$f" ] && fail "temp file left behind: $f"; done
echo "ok 4 tampered asset refused"

# 5. No checksum for this platform -> clear error, nothing downloaded.
: >"$work/plugin/bin/SHA256SUMS"
if out=$("$launcher" 2>&1); then fail "ran without a checksum"; fi
case "$out" in *"no checksum"*) ;; *) fail "unexpected: $out" ;; esac
echo "ok 5 missing checksum refused"

# 6. AGY_PLUGIN_BIN overrides everything (dev builds).
out=$(AGY_PLUGIN_BIN=/bin/echo "$launcher" dev-mode) || fail "override failed"
[ "$out" = "dev-mode" ] || fail "override output: $out"
echo "ok 6 AGY_PLUGIN_BIN override"
echo "launcher: all tests passed"
