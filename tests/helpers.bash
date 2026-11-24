#!/usr/bin/env bash
# Shared helpers for the bats suite.
# Sources agy-run.sh in "library mode" — the script has a sourcing
# guard (`if [ "${BASH_SOURCE[0]:-}" = "${0:-}" ]; then main "$@"; fi`)
# so functions become callable without triggering main().

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGY_RUN="${REPO_ROOT}/plugins/agy/scripts/agy-run.sh"

# Source the wrapper into the current shell. Bats runs each @test in a
# subshell, so this is per-test scoped.
#
# The wrapper sets `set -euo pipefail` at the top. Keep errexit ON: bats
# decides pass/fail through it, so a failing `[ ... ]` assertion must abort
# the @test body. (Until 0.9.0 this helper ran `set +e`, which made every
# assertion in a test that sourced the wrapper a no-op — those tests could
# not fail.) Expected non-zero exits go through `run`, which captures them.
# Only the wrapper's nounset/pipefail are dropped, to keep bats' own
# internals safe.
load_agy_run() {
  # shellcheck disable=SC1090
  source "$AGY_RUN"
  set -e
  set +u +o pipefail
}

# Run a function from agy-run.sh and capture stdout + exit code into
# the standard bats globals ($status, $output).
run_agy_fn() {
  load_agy_run
  run "$@"
}
