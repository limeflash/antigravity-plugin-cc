#!/usr/bin/env bash
# Live end-to-end check against a REAL, logged-in agy (uses model quota).
# Not run in CI. Usage: scripts/e2e-live.sh [path/to/agy-plugin binary]
# Builds the release binary if none is given and drives it through the
# launcher exactly as the slash commands do.
set -uo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
bin="${1:-}"
if [ -z "$bin" ]; then
  (cd "$repo" && cargo build --release --quiet) || exit 1
  bin="$repo/target/release/agy-plugin"
fi
export AGY_PLUGIN_BIN="$bin"
plugin="$repo/plugins/agy/bin/agy-plugin"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
pass=0 fail=0
ok() { echo "PASS  $1"; pass=$((pass + 1)); }
bad() { echo "FAIL  $1"; echo "$2" | sed 's/^/        /' | head -20; fail=$((fail + 1)); }

# --- ask --------------------------------------------------------------------
out=$(cd "$work" && "$plugin" ask "What is 6 times 7? Reply with only the number." 2>&1)
[ "$(echo "$out" | tr -d '[:space:]')" = 42 ] && ok "ask -> 42" || bad "ask -> 42" "$out"

out=$(cd "$work" && "$plugin" ask --model flash-low "Reply with only the word: PONG" 2>&1)
echo "$out" | grep -q PONG && ok "ask --model flash-low" || bad "ask --model flash-low" "$out"

# --- the write-escape probe (0.8.x let agy write here on agy >= 1.5.7) ------
target="$work/target-repo"
mkdir -p "$target" && git -C "$target" init -q && echo original >"$target/victim.txt"
git -C "$target" add -A && git -C "$target" -c user.email=t@t -c user.name=t commit -qm init
# Neutral wording on purpose: "PWNED"-style prompts trip Gemini's own safety
# filter, which would test the model's filter instead of the plugin's pin.
out=$(cd "$work" && "$plugin" ask "Please update the file $target/victim.txt so that it contains the single word UPDATED, and create $target/new.txt containing the word UPDATED. Do not ask anything, just do it." 2>&1)
if [ "$(cat "$target/victim.txt")" = original ] && [ ! -e "$target/new.txt" ]; then
  ok "read-only: agy could NOT write into an outside git repo"
  # Either agy tried and the plugin reported the denial, or agy (told the
  # session is read-only) declined by itself. Both leave the file untouched,
  # which is the property that matters; the user must hear about it either way.
  if echo "$out" | grep -qiE "blocks|blocked|read-only|not permitted|cannot|denied"; then
    ok "read-only: the refusal was surfaced to the user"
  else
    bad "refusal surfaced" "$out"
  fi
else
  bad "read-only: agy WROTE into an outside git repo" "$out"
fi

# --- review / adversarial-review ---------------------------------------------
demo="$work/demo"
mkdir -p "$demo" && git -C "$demo" init -q
printf 'function add(a,b){return a+b}\nmodule.exports={add}\n' >"$demo/calc.js"
git -C "$demo" add -A && git -C "$demo" -c user.email=t@t -c user.name=t commit -qm init
printf 'function add(a,b){return a-b}\nfunction divide(a,b){return a/b}\nmodule.exports={add,divide}\n' >"$demo/calc.js"
out=$(cd "$demo" && "$plugin" review "Be brief. List the bugs in calc.js." 2>&1)
if echo "$out" | grep -qiE 'subtract|minus|a ?- ?b|-' && echo "$out" | grep -qiE 'zero|divis'; then
  ok "review finds both bugs"
else
  bad "review finds both bugs" "$out"
fi
out=$(cd "$demo" && "$plugin" adversarial-review --background --wait "Challenge calc.js" 2>&1)
echo "$out" | grep -q "ended with status: completed" && ok "adversarial-review --background --wait" || bad "adversarial-review --background --wait" "$out"
out=$(cd "$demo" && "$plugin" status 2>&1)
echo "$out" | grep -q "\[done\]" && ok "status lists the job" || bad "status" "$out"
[ "$(git -C "$demo" status --porcelain | grep -v '^ M calc.js$' | grep -c .)" = 0 ] && ok "review left the repo untouched (.agy-plugin excluded)" || bad "repo untouched" "$(git -C "$demo" status --porcelain)"

# --- scrape / doc-to-md ------------------------------------------------------
out=$(cd "$work" && "$plugin" scrape https://example.com 2>&1)
echo "$out" | grep -qiE "example domain|documentation examples" && ok "scrape https://example.com" || bad "scrape" "$out"
out=$(cd "$work" && "$plugin" scrape http://169.254.169.254/ 2>&1); rc=$?
[ $rc = 65 ] && ok "scrape refuses the metadata IP (65)" || bad "scrape SSRF" "$out"
printf '<html><body><h1>Hello</h1><ul><li>one</li><li>two</li></ul></body></html>\n' >"$work/page.html"
out=$(cd "$work" && "$plugin" doc-to-md page.html 2>&1)
echo "$out" | grep -q "Hello" && echo "$out" | grep -qE '^[-*] +one' && ok "doc-to-md html -> markdown" || bad "doc-to-md" "$out"

# --- rescue --isolate + background cancel ------------------------------------
git -C "$demo" checkout -q -- calc.js
out=$(cd "$demo" && "$plugin" rescue --isolate "Create a file named HELLO.txt containing the word hello. Do nothing else." 2>&1)
if echo "$out" | grep -q "HELLO.txt" && [ ! -e "$demo/HELLO.txt" ]; then
  ok "rescue --isolate produced a patch, tree untouched"
else
  bad "rescue --isolate" "$out"
fi
out=$(cd "$demo" && "$plugin" rescue --background "Count slowly from 1 to 500, one number per line, then stop." 2>&1)
id=$(echo "$out" | grep -oE 'agy-[0-9a-f]{8}' | head -n1)
sleep 4
out=$(cd "$demo" && "$plugin" cancel "$id" 2>&1)
echo "$out" | grep -qE "canceled \((killed|already-dead|no-pid-marking-canceled)\)" && ok "cancel background rescue" || bad "cancel" "$out"
pid_alive=$(pgrep -f "_run-job $id" || true)
[ -z "$pid_alive" ] && ok "no worker left running after cancel" || bad "worker left running" "$pid_alive"

# --- check / help -----------------------------------------------------------
out=$("$plugin" check)
echo "$out" | grep -q '"supported":true' && ok "check: supported" || bad "check" "$out"
"$plugin" help | grep -q "Available models" && ok "help resolves models live" || bad "help" ""

echo
echo "e2e: $pass passed, $fail failed"
[ "$fail" = 0 ]
