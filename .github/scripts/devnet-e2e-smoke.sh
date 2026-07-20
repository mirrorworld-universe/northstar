#!/usr/bin/env bash
set -euo pipefail
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"

SOLANA_CLI=${SOLANA_CLI:-/solana/northstar/release/solana}
DEPLOYER_KEYPAIR=${DEPLOYER_KEYPAIR:-/home/ubuntu/.config/solana/id.json}
DEVNET_RPC=${DEVNET_RPC:-https://api.devnet.solana.com}
L1_LOCAL_RPC=${L1_LOCAL_RPC:-http://localhost:8899}
ER_RPC=${ER_RPC:-http://localhost:8910}
AMOUNT_SOL=${AMOUNT_SOL:-0.01}
AMOUNT_LAMPORTS=${AMOUNT_LAMPORTS:-10000000}
REPORT_PATH=${REPORT_PATH:-/tmp/northstar-e2e-report.txt}
SERVICE=${SERVICE:-northstar-validator}
PORTAL_ADDRESS=${PORTAL_ADDRESS:-}

for command in curl jq solana-keygen systemctl; do
  command -v "$command" >/dev/null
done

STARTED_AT=$(date +%s)
STATUS=failure
PHASE=initializing
DEPOSIT_SIGNATURE=
WITHDRAWAL_SIGNATURE=
SETTLEMENT_SIGNATURES=
SESSION_PDA=
VALIDATOR_IDENTITY=
WALLET_ADDRESS=

write_report() {
  local exit_code=$?
  trap - EXIT
  local elapsed=$(( $(date +%s) - STARTED_AT ))
  if [ "$exit_code" -eq 0 ]; then
    STATUS=success
  fi
  # shellcheck disable=SC2016
  {
    printf ':test_tube: Devnet E2E smoke status: *%s*\n' "$STATUS"
    printf ':stopwatch: Elapsed: %ss\n' "$elapsed"
    printf ':triangular_flag_on_post: Last phase: `%s`\n' "$PHASE"
    printf ':identification_card: Validator: `%s`\n' "${VALIDATOR_IDENTITY:-unknown}"
    printf ':link: Portal: https://explorer.solana.com/address/%s?cluster=devnet\n' "${PORTAL_ADDRESS:-unknown}"
    printf ':key: Session: `%s`\n' "${SESSION_PDA:-unknown}"
    printf ':inbox_tray: L1 deposit: https://explorer.solana.com/tx/%s?cluster=devnet\n' "${DEPOSIT_SIGNATURE:-unavailable}"
    printf ':outbox_tray: ER withdrawal: https://explorer.solana.com/tx/%s?cluster=custom&customUrl=https%%3A%%2F%%2Fephemeral.devnet.sonic.game\n' "${WITHDRAWAL_SIGNATURE:-unavailable}"
    printf ':classical_building: L1 settlement transactions: %s\n' "${SETTLEMENT_SIGNATURES:-unavailable}"
  } > "$REPORT_PATH"
  cat "$REPORT_PATH"
  exit "$exit_code"
}
trap write_report EXIT

rpc_result() {
  local url=$1 method=$2 params=$3
  curl -sSf --max-time 15 -X POST -H 'Content-Type: application/json' \
    --data "$(jq -cn --arg method "$method" --argjson params "$params" \
      '{jsonrpc:"2.0",id:1,method:$method,params:$params}')" \
    "$url" | jq -e '.result'
}

get_balance() {
  local url=$1 address=$2 commitment=$3
  rpc_result "$url" getBalance "$(jq -cn --arg address "$address" --arg commitment "$commitment" \
    '[$address,{commitment:$commitment}]')" | jq -r '.value'
}

get_session() {
  rpc_result "$ER_RPC" getSessionPda '[]' | jq -r '. // empty'
}

wait_for_l1_rpc() {
  for _ in $(seq 1 1080); do
    if sudo systemctl is-failed --quiet "$SERVICE"; then
      echo "Validator service failed" >&2
      return 1
    fi
    if sudo systemctl is-active --quiet "$SERVICE" && \
      rpc_result "$L1_LOCAL_RPC" getVersion '[]' >/dev/null 2>&1; then
      return 0
    fi
    sleep 5
  done
  echo "Timed out waiting for L1 RPC" >&2
  return 1
}

wait_for_l1_catchup() {
  for _ in $(seq 1 360); do
    local local_slot public_slot gap
    local_slot=$(rpc_result "$L1_LOCAL_RPC" getSlot '[{"commitment":"confirmed"}]' 2>/dev/null || true)
    public_slot=$(rpc_result "$DEVNET_RPC" getSlot '[{"commitment":"confirmed"}]' 2>/dev/null || true)
    if [[ "$local_slot" =~ ^[0-9]+$ ]] && [[ "$public_slot" =~ ^[0-9]+$ ]]; then
      gap=$((public_slot - local_slot))
      if [ "$gap" -ge -50 ] && [ "$gap" -le 50 ]; then
        return 0
      fi
    fi
    sleep 5
  done
  echo "Timed out waiting for local L1 RPC to catch up with devnet" >&2
  return 1
}

wait_for_session() {
  local expected=$1
  for _ in $(seq 1 360); do
    local actual
    actual=$(get_session 2>/dev/null || true)
    if [ "$actual" = "$expected" ]; then
      return 0
    fi
    sleep 5
  done
  echo "Timed out waiting for ER session $expected" >&2
  return 1
}

wait_for_any_session() {
  for _ in $(seq 1 360); do
    local actual
    actual=$(get_session 2>/dev/null || true)
    if [ -n "$actual" ]; then
      printf '%s\n' "$actual"
      return 0
    fi
    sleep 5
  done
  echo "Timed out waiting for an active ER session" >&2
  return 1
}

wait_for_balance() {
  local url=$1 address=$2 commitment=$3 expected=$4 label=$5
  for _ in $(seq 1 180); do
    local actual
    actual=$(get_balance "$url" "$address" "$commitment" 2>/dev/null || true)
    if [ "$actual" = "$expected" ]; then
      return 0
    fi
    sleep 5
  done
  echo "Timed out waiting for $label balance $expected" >&2
  return 1
}

wait_for_deposit_credit() {
  local baseline=$1
  local materialized=$AMOUNT_LAMPORTS
  local accumulated=$((baseline + AMOUNT_LAMPORTS))
  for _ in $(seq 1 180); do
    local actual
    actual=$(get_balance "$ER_RPC" "$WALLET_ADDRESS" processed 2>/dev/null || true)
    if [ "$actual" = "$materialized" ] || [ "$actual" = "$accumulated" ]; then
      printf '%s\n' "$actual"
      return 0
    fi
    sleep 5
  done
  echo "Timed out waiting for ER deposit credit ($materialized or $accumulated)" >&2
  return 1
}

PHASE=resolve_configuration
test -x "$SOLANA_CLI"
test -f "$DEPLOYER_KEYPAIR"
WALLET_ADDRESS=$(solana-keygen pubkey "$DEPLOYER_KEYPAIR")
if [ -z "$PORTAL_ADDRESS" ]; then
  PORTAL_ADDRESS=$(systemctl cat "$SERVICE" | sed -n 's/.*--portal \([^ ]*\).*/\1/p' | tail -n 1)
fi
test -n "$PORTAL_ADDRESS"
VALIDATOR_IDENTITY=$(rpc_result "$L1_LOCAL_RPC" getIdentity '[]' | jq -r '.identity')
SESSION_PDA=$(wait_for_any_session)
wait_for_l1_catchup

PHASE=capture_baseline
BASELINE_ER_BALANCE=$(get_balance "$ER_RPC" "$WALLET_ADDRESS" processed)
LOG_START_LINE=$(wc -l < /solana/northstar/logs/validator.log)
rpc_result "$DEVNET_RPC" getSignaturesForAddress \
  "$(jq -cn --arg portal "$PORTAL_ADDRESS" '[$portal,{limit:40,commitment:"confirmed"}]')" \
  | jq -r '.[].signature' > /tmp/northstar-e2e-before-signatures.txt

PHASE=deposit_l1
DEPOSIT_OUTPUT=$(
  "$SOLANA_CLI" \
    --url "$DEVNET_RPC" \
    --keypair "$DEPLOYER_KEYPAIR" \
    --commitment confirmed \
    --output json-compact \
    portal deposit-fee \
    --portal "$PORTAL_ADDRESS" \
    "$AMOUNT_SOL"
)
DEPOSIT_SIGNATURE=$(jq -er '.signature' <<< "$DEPOSIT_OUTPUT")
POST_DEPOSIT_L1_BALANCE=$(get_balance "$DEVNET_RPC" "$WALLET_ADDRESS" confirmed)

PHASE=wait_for_er_credit
CREDITED_BALANCE=$(wait_for_deposit_credit "$BASELINE_ER_BALANCE")

PHASE=restart_with_unsettled_state
sudo systemctl restart "$SERVICE"
wait_for_l1_rpc
wait_for_l1_catchup
wait_for_session "$SESSION_PDA"

PHASE=verify_restored_state
RESTORED_BALANCE=$(get_balance "$ER_RPC" "$WALLET_ADDRESS" processed)
if [ "$RESTORED_BALANCE" != "$CREDITED_BALANCE" ]; then
  echo "Restored ER balance mismatch: expected=$CREDITED_BALANCE actual=$RESTORED_BALANCE" >&2
  exit 1
fi

PHASE=withdraw_er
WITHDRAWAL_OUTPUT=$(
  "$SOLANA_CLI" \
    --url "$ER_RPC" \
    --keypair "$DEPLOYER_KEYPAIR" \
    --commitment confirmed \
    --output json-compact \
    portal withdraw-fee \
    --portal "$PORTAL_ADDRESS" \
    "$AMOUNT_SOL"
)
WITHDRAWAL_SIGNATURE=$(jq -er '.signature' <<< "$WITHDRAWAL_OUTPUT")
EXPECTED_WITHDRAWN_BALANCE=$((CREDITED_BALANCE - AMOUNT_LAMPORTS))
wait_for_balance "$ER_RPC" "$WALLET_ADDRESS" processed "$EXPECTED_WITHDRAWN_BALANCE" 'ER withdrawal'

PHASE=wait_for_l1_settlement
EXPECTED_L1_BALANCE=$((POST_DEPOSIT_L1_BALANCE + AMOUNT_LAMPORTS))
for _ in $(seq 1 180); do
  CURRENT_L1_BALANCE=$(get_balance "$DEVNET_RPC" "$WALLET_ADDRESS" confirmed 2>/dev/null || true)
  if [ -n "$CURRENT_L1_BALANCE" ] && [ "$CURRENT_L1_BALANCE" -ge "$EXPECTED_L1_BALANCE" ]; then
    break
  fi
  sleep 5
done
CURRENT_L1_BALANCE=$(get_balance "$DEVNET_RPC" "$WALLET_ADDRESS" confirmed)
if [ "$CURRENT_L1_BALANCE" -lt "$EXPECTED_L1_BALANCE" ]; then
  echo "Settlement did not return $AMOUNT_LAMPORTS lamports to L1" >&2
  exit 1
fi

PHASE=collect_settlement_receipts
CONFIRMED_SIGNATURES=$(
  tail -n "+$((LOG_START_LINE + 1))" /solana/northstar/logs/validator.log \
    | sed -n 's/.*Portal settlement transaction confirmed.*signatures=\[\([^]]*\)\].*/\1/p' \
    | tr ',' '\n' \
    | tr -d ' []"' \
    | tail -n 3
 )
if [ -z "$CONFIRMED_SIGNATURES" ]; then
  rpc_result "$DEVNET_RPC" getSignaturesForAddress \
    "$(jq -cn --arg portal "$PORTAL_ADDRESS" '[$portal,{limit:40,commitment:"confirmed"}]')" \
    | jq -r '.[].signature' > /tmp/northstar-e2e-after-signatures.txt
  CONFIRMED_SIGNATURES=$(
    grep -Fvx -f /tmp/northstar-e2e-before-signatures.txt /tmp/northstar-e2e-after-signatures.txt \
      | grep -Fvx "$DEPOSIT_SIGNATURE" \
      | tail -n 3
  )
fi
SETTLEMENT_SIGNATURES=$(
  awk '{printf "<https://explorer.solana.com/tx/%s?cluster=devnet|%s> ", $1, substr($1,1,8)}' \
    <<< "$CONFIRMED_SIGNATURES"
 )
test -n "$SETTLEMENT_SIGNATURES"

PHASE=complete
