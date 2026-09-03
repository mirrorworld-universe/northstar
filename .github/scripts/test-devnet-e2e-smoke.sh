#!/usr/bin/env bash
set -euo pipefail

script=${1:-.github/scripts/devnet-e2e-smoke.sh}
# Match literal variable references in the script under test.
# shellcheck disable=SC2016
deposit_line=$(grep -n '^DEPOSIT_SIGNATURE=$(jq' "$script" | cut -d: -f1)
# shellcheck disable=SC2016
finalization_line=$(grep -n '^wait_for_signature_finalization "\$L1_LOCAL_RPC" "\$DEPOSIT_SIGNATURE"' "$script" | cut -d: -f1)
restart_line=$(grep -n '^restart_validator$' "$script" | cut -d: -f1)
# shellcheck disable=SC2016
restored_receipt_line=$(grep -n '^wait_for_account_owner "\$L1_LOCAL_RPC" "\$DEPOSIT_RECEIPT_PDA" "\$PORTAL_ADDRESS"' "$script" | cut -d: -f1)

test -n "$deposit_line"
test -n "$finalization_line"
test -n "$restart_line"
test -n "$restored_receipt_line"
test "$deposit_line" -lt "$finalization_line"
test "$finalization_line" -lt "$restart_line"
test "$restart_line" -lt "$restored_receipt_line"

eval "$(sed -n '/^wait_for_signature_finalization() {$/,/^}$/p' "$script")"
eval "$(sed -n '/^deposit_receipt_address() {$/,/^}$/p' "$script")"
eval "$(sed -n '/^wait_for_account_owner() {$/,/^}$/p' "$script")"

PORTAL_ADDRESS=Portal1111111111111111111111111111111111111
export DEVNET_RPC=http://devnet.invalid
expected_receipt=Receipt11111111111111111111111111111111111

rpc_result() {
  case $2 in
    getSignatureStatuses)
      printf '%s\n' '{"value":[{"confirmationStatus":"finalized","err":null}]}'
      ;;
    getTransaction)
      printf '%s\n' '{"transaction":{"message":{"accountKeys":["Payer11111111111111111111111111111111111111","Receipt11111111111111111111111111111111111","Portal1111111111111111111111111111111111111"],"instructions":[{"programIdIndex":2,"accounts":[0,0,1]}]}}}'
      ;;
    getAccountInfo)
      printf '%s\n' '{"value":{"owner":"Portal1111111111111111111111111111111111111"}}'
      ;;
    *)
      return 1
      ;;
  esac
}

wait_for_signature_finalization http://local.invalid signature
test "$(deposit_receipt_address signature)" = "$expected_receipt"
wait_for_account_owner http://local.invalid "$expected_receipt" "$PORTAL_ADDRESS"
