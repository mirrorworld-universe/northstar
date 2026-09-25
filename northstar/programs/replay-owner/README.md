# Replay owner fixture

This test-only SBF program connects fresh owner-program delegation to Northstar's frozen one-byte replay profile. It has no application authority model and must never hold real assets.

- Instruction `0 || grid_id_le_u64` copies an eight-byte target into an owner-owned buffer, zeros the target, assigns it to Portal, and invokes `Delegate`. The target and payer sign. The fixture deliberately uses ordinary signed accounts, not PDAs.
- Instruction `[1]` writes byte `100` to the target's first byte. It uses exactly one memcpy syscall because the frozen replay relation requires that syscall shape. The guest and proof ABI are unchanged.

Build from the repository root:

```sh
cargo build-sbf --manifest-path northstar/programs/replay-owner/Cargo.toml
NORTHSTAR_LIVE_OWNER_SBF="$PWD/target/deploy/northstar_replay_owner.so" \
  cargo test -p northstar fresh_delegation_owner_preserves_frozen_replay_relation -- --ignored
```

The latter test executes SBF, reconstructs the captured witness, checks native replay, and exercises changed-field rejection. It is not a GPU proof. `live_fresh_delegation_cpi` additionally checks actual L1 creation, owner-program CPI, record creation, and account conservation on a fresh test validator. The live checkpoint proof test accepts `NORTHSTAR_LIVE_OWNER_SBF` to use these freshly delegated accounts instead of genesis delegation fixtures. The [`manager-recovery` runner](../../docs/gpu-proof-to-settlement-v1.md#fresh-delegation-and-automatic-manager-recovery) combines that flow with proof resolution and observation-only validation of automatic settlement after a snapshot-fenced restart.
