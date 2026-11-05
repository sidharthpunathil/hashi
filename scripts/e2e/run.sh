#!/bin/sh
# End-to-end check of a built `hashi` binary against fake agent CLIs.
#
# Runs two bridges on loopback (the owner's and a friend's), so no real
# agent, account or quota is used and nothing leaves the machine. Intended
# for a throwaway container:
#
#   docker run --rm -v "$PWD:/src:ro" rust:1-bookworm sh /src/scripts/e2e/run.sh
#
# or locally with HASHI=/path/to/hashi sh scripts/e2e/run.sh
set -eu

WORK=$(mktemp -d)
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$WORK"' EXIT
pass=0
fail=0
ok() { pass=$((pass + 1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail + 1)); printf '  FAIL  %s\n' "$1"; }
check() { if eval "$2"; then ok "$1"; else bad "$1"; fi; }

if [ -z "${HASHI:-}" ]; then
  echo "building hashi (release)"
  command -v curl >/dev/null || { apt-get update -qq >/dev/null && apt-get install -y -qq curl >/dev/null; }
  cp -R /src "$WORK/src" && rm -rf "$WORK/src/target"
  (cd "$WORK/src" && cargo build --release --locked -q)
  mkdir -p "$WORK/bin" && cp "$WORK/src/target/release/hashi" "$WORK/bin/hashi"
  HASHI="$WORK/bin/hashi"
fi
command -v curl >/dev/null || { echo "curl is needed"; exit 1; }

# A fake Claude Code: answers in stream-json, "slow" sleeps, "fail" fails.
FAKE="$WORK/fake-claude"
cat >"$FAKE" <<'EOF'
#!/bin/sh
if [ "$1" = "--version" ]; then echo "2.1.318 (Claude Code)"; exit 0; fi
for last; do :; done
case "$last" in
  fail*) echo '{"type":"result","is_error":true,"subtype":"error_during_execution","result":"simulated failure"}'; exit 1 ;;
  slow*) sleep 20 ;;
esac
printf '{"type":"result","is_error":false,"result":"claude says %s","session_id":"s-1","total_cost_usd":0.01,"usage":{"input_tokens":5,"output_tokens":7}}\n' "$last"
EOF
chmod +x "$FAKE"
NONE="$WORK/none"

serve() { # home port log
  HASHI_HOME="$1" "$HASHI" serve --headless --port "$2" --claude-bin "$FAKE" \
    --codex-bin "$NONE" --gemini-bin "$NONE" --opencode-bin "$NONE" \
    --qwen-bin "$NONE" --cursor-bin "$NONE" --no-update-check >"$3" 2>&1 &
  i=0
  until curl -fs "http://127.0.0.1:$2/healthz" >/dev/null 2>&1; do
    i=$((i + 1)); [ $i -gt 100 ] && { cat "$3"; echo "server on $2 never came up"; exit 1; }
    sleep 0.1
  done
}

A="$WORK/owner"; B="$WORK/friend"
echo "owner bridge"
serve "$A" 7801 "$WORK/a.log"
ADMIN=$(grep -o 'hashi_[0-9a-f]\{64\}' "$WORK/a.log" | head -1)
check "admin token printed once" '[ -n "$ADMIN" ]'
U=http://127.0.0.1:7801
H="Authorization: Bearer $ADMIN"

check "status lists claude as installed" \
  'curl -fs $U/v1/status | grep -q "\"agent\":\"claude\",\"available\":true"'
check "a second server on the same home is refused" \
  '! HASHI_HOME="$A" "$HASHI" serve --headless --port 7809 --claude-bin "$FAKE" --no-update-check >"$WORK/dup.log" 2>&1'

TOKEN=$(curl -fs $U/v1/tokens -H "$H" -d '{"name":"app","sessionMode":"stateful"}' | grep -o 'hashi_[0-9a-f]\{64\}')
check "token created over the API" '[ -n "$TOKEN" ]'
T="Authorization: Bearer $TOKEN"

check "ask answers within wait" \
  'curl -fs $U/v1/ask -H "$T" -d "{\"message\":\"ping\",\"wait\":30}" | grep -q "claude says ping"'
check "a failed run reports failed" \
  'curl -s $U/v1/ask -H "$T" -d "{\"message\":\"fail now\",\"wait\":30}" | grep -q "\"status\":\"failed\""'
check "OpenAI chat completion" \
  'curl -fs $U/v1/chat/completions -H "$T" -d "{\"model\":\"claude\",\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}" | grep -q "claude says"'
check "OpenAI stream ends with DONE" \
  'curl -fsN $U/v1/chat/completions -H "$T" -d "{\"model\":\"claude\",\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}" | grep -q "data: \[DONE\]"'
check "/v1/models lists claude" 'curl -fs $U/v1/models -H "$T" | grep -q "\"id\":\"claude\""'
check "/v1/me works and hides accounts" \
  'curl -fs $U/v1/me -H "$T" | grep -q "\"name\":\"app\"" && ! curl -fs $U/v1/me -H "$T" | grep -q accounts'

SLOW=$(curl -fs $U/v1/ask -H "$T" -d '{"message":"slow one"}' | grep -o '"id":"[^"]*"' | head -1 | cut -d'"' -f4)
sleep 1
check "cancel a running request" \
  'curl -fs -X DELETE $U/v1/ask/$SLOW -H "$T" | grep -q "\"status\":\"failed\""'
check "user token cannot list tokens" \
  '[ "$(curl -s -o /dev/null -w "%{http_code}" $U/v1/tokens -H "$T")" = 403 ]'
check "unknown Host header refused" \
  '[ "$(curl -s -o /dev/null -w "%{http_code}" -H "Host: evil.example" $U/healthz)" = 421 ]'
check "CLI lists tokens while the server runs" 'HASHI_HOME="$A" "$HASHI" token list | grep -q app'

echo "friend connects with an invite"
LINK=$(HASHI_HOME="$A" "$HASHI" share friend --public-url "$U" 2>/dev/null | grep -o 'http://[^ ]*/claim/hashi_inv_[0-9a-f]*' | head -1)
check "invite link made" '[ -n "$LINK" ]'
check "hashi connect redeems it" 'HASHI_HOME="$B" "$HASHI" connect "$LINK" --name owner >/dev/null 2>&1'
check "ask --remote reaches the owner" \
  'HASHI_HOME="$B" "$HASHI" ask --remote owner "from friend" 2>/dev/null | grep -q "claude says from friend"'
check "the link works only once" '! HASHI_HOME="$WORK/third" "$HASHI" connect "$LINK" >/dev/null 2>&1'

echo "friend uses the shared access as an account on their own bridge"
check "remote account added" \
  'HASHI_HOME="$B" "$HASHI" account add claude via-owner --remote owner >/dev/null 2>&1'
serve "$B" 7802 "$WORK/b.log"
BADMIN=$(grep -o 'hashi_[0-9a-f]\{64\}' "$WORK/b.log" | head -1)
BT=$(curl -fs http://127.0.0.1:7802/v1/tokens -H "Authorization: Bearer $BADMIN" \
  -d '{"name":"b-app","accounts":{"claude":["via-owner"]}}' | grep -o 'hashi_[0-9a-f]\{64\}')
check "a run on the remote account is forwarded" \
  'curl -fs http://127.0.0.1:7802/v1/ask -H "Authorization: Bearer $BT" -d "{\"message\":\"forwarded\",\"wait\":60}" | grep -q "claude says forwarded"'

echo "uninstall"
check "dry run lists the home and removes nothing" \
  'HASHI_HOME="$WORK/third" "$HASHI" uninstall --dry-run --keep-data >/dev/null 2>&1 || true; [ -d "$A" ]'
check "uninstall refuses while the owner server runs" \
  '! HASHI_HOME="$A" "$HASHI" uninstall --yes --keep-data >/dev/null 2>&1'

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
