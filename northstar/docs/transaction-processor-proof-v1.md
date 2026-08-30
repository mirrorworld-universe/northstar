# Full transaction-processor proof specification v1

Status: frozen. Scaled fixtures may add rows but must not change this contract.

This document defines proof kind `2`, version `1` of `northstar-er-step-v1`. It keeps the Portal-facing eight-field ABI byte-for-byte compatible with `ErStepPublicInputsV1`; proof kind `1` remains the one-account feasibility circuit.

## Statement

A valid proof establishes that one serialized Solana transaction, executed at one ER step under the committed runtime and bank inputs, deterministically produces the committed result and post-state. The processor either follows a supported branch completely or rejects before proving. No unsupported behavior may be represented as a successful proof.

The proved transition starts with transaction decoding and sanitization and ends with selection of committed or rollback accounts. Locks, scheduling, cache replacement, persistence mechanics, notifications, and metrics are outside the transition.

## Public inputs

The eight canonical BN254 scalar inputs retain their existing order and encoding:

1. `domain`: `FrBytes::er_step_domain_v1(2, 1)`.
2. `session_context`: Poseidon commitment to chain identity, Portal session, ER identity, and settlement policy version.
3. `slot_step`: big-endian packed `(er_slot, step_index)`.
4. `pre_state_root`: root of all mutable ER accounts before the transaction.
5. `post_state_root`: root after applying the selected commit or rollback effects.
6. `tx_effect_root`: `Poseidon(TX_EFFECT_V1, transaction_commitment, runtime_commitment, result_commitment, trace_schema_commitment, settlement_effect_root)`.
7. `readonly_l1_root`: root binding readonly L1 accounts and their observed slots/versions.
8. `settlement_effect_root`: ordered commitment to committed account effects visible to settlement.

All sub-commitments use typed domain tags and length-delimited field chunking. Byte strings are split into 31-byte big-endian chunks; the committed sequence includes byte length before chunks. Lists commit their element count and preserve the canonical order stated below. No digest is silently reduced modulo BN254 Fr.

### Transaction commitment

Commits, in order, to:

- exact wire bytes and transaction version;
- signatures in wire order;
- message hash;
- resolved static and lookup-table account keys;
- signer, writable, invoked, and instruction-account flags per resolved key;
- recent blockhash;
- compiled instructions in message order.

### Runtime commitment

Commits, in order, to:

- proof-spec and trace-schema versions;
- Agave revision and Northstar runtime revision;
- active feature-set bitmap/identifiers;
- SBPF version and VM configuration;
- syscall registry commitment;
- builtin registry commitment;
- fee structure, rent, slot, epoch, and epoch stake inputs;
- recent-blockhash queue commitment;
- sysvar-cache commitment;
- loaded program ELF and loader-state commitments;
- address-lookup-table inputs.

Host architecture, JIT addresses, cache residency, timing, and metrics are forbidden inputs.

### Result commitment

Commits, in order, to:

- outcome class: unprocessable, no-op, fees-only, executed-success, or executed-failure;
- canonical transaction/instruction/custom error numbers and instruction index;
- executed compute units and loaded-account data size;
- transaction and prioritization fees;
- return data and log commitment;
- ordered committed-account effects;
- ordered rollback-account effects.

Account entries are ordered by resolved transaction index, then by pubkey for externally loaded entries. Each entry commits pubkey, lamports, owner, executable bit, rent epoch, data length, and data bytes.

## Boundary classification

| Surface | v1 classification | Binding or rule |
|---|---|---|
| Wire decoding, size/version checks, sanitization | Proved | Exact transaction bytes and runtime version |
| Message hash | Proved | Exact wire message bytes |
| Ed25519 transaction signatures | Coprocessor-backed | Verified signature claim is bound to signer and message hash |
| Secp256k1, Ed25519, secp256r1 precompiles | Coprocessor-backed | Inputs, output/error, feature gate, and cost are bound |
| Address lookup resolution | Proved | Table accounts are external-but-bound inputs |
| Reserved-key and instruction-count checks | Proved | Feature/runtime commitment |
| Account locks and scheduler ordering | External-but-bound | Step index and pre-state root establish serial order |
| Status-cache duplicate check | Unsupported | Fail closed; dispute step proves first execution only |
| Blockhash age and durable nonce validation | Proved | Blockhash queue and nonce account are bound |
| Compute-budget parsing and fee calculation | Proved | Fee/runtime inputs are bound |
| Fee-payer validation and debit | Proved | Fee payer is in pre/post or rollback state |
| Account, sysvar, and program loading | Proved | All loaded values are bound; cache mechanics excluded |
| ELF parsing/verification | Proved | ELF bytes, loader, SBPF version, and VM config are bound |
| Builtin programs | Unsupported unless listed in runtime commitment | Unknown builtin fails closed |
| SBPF instructions and memory | Proved | Canonical VM trace/witness under committed SBPF version |
| Syscalls | Proved or coprocessor-backed per registry entry | Unknown syscall fails closed |
| CPI, signer derivation, and privilege checks | Proved | Call tree and account effects are bound |
| Account ownership, rent, resize, and lamport invariants | Proved | Pre/post account states are bound |
| Program deployment/upgrade/close | Unsupported in v1 fixtures | Fail closed pending program-cache state model |
| Vote/stake builtins and epoch rewards | Unsupported in v1 | Fail closed |
| Commit-versus-rollback selection | Proved | Outcome and both effect lists are bound |
| AccountsDB writes, LT-hash update, status notifications | External-but-bound | Post-state/effect roots bind semantic write set |
| Cache eviction, cooperative loading, timings, logs transport, metrics | Excluded artifact | Must not affect trace bytes or proof result |

`Unsupported` is a protocol result, not an execution error. A prover must emit no proof for such a branch. Verifiers reject unknown proof kinds, versions, runtime registries, trace versions, event tags, enum values, trailing bytes, and noncanonical field encodings.

## Canonical execution path

Current Northstar master (`d1cc0114f8`) maps the boundary as follows:

1. `runtime/src/bank.rs::verify_transaction_with_serialized_message`: wire size, version, signatures/message hash, sanitization, lookup resolution.
2. `runtime/src/bank/check_transactions.rs::check_transactions_with_processed_slots`: version gate, locks result, blockhash/nonce age, compute budget, fees, duplicate status.
3. `runtime/src/bank.rs::load_and_execute_transactions`: constructs committed environment and enters SVM.
4. `svm/src/transaction_processor.rs::load_and_execute_sanitized_transactions`: fee payer/nonce validation, account/program loading, execution, and batch-local rollback selection.
5. `program-runtime/src/invoke_context.rs::process_message`: top-level instruction sequence, precompiles, builtins, CPI, and account privileges.
6. `program-runtime/src/vm.rs::execute`: SBPF VM, memory mapping, compute meter, syscalls, and parameter deserialization.
7. `svm/src/transaction_processor.rs::execute_loaded_transaction`: post-execution account/rent/lamport checks and execution result.
8. `runtime/src/bank.rs::commit_transactions`: selects successful or rollback accounts and derives committed result.

The Linear links to `svm/src/message_processor.rs` describe the older layout. Agave moved that logic into `InvokeContext::process_message`.

## Trace relationship

Trace schema v1 is a deterministic conformance and witness interchange format, not an additional public input. Its canonical SHA-256 hash detects fixture drift. Soundness comes from proving processor semantics against the public transaction/runtime/result/state commitments; a prover cannot substitute a different trace that violates those semantics.

Trace v1 records processor stages, canonical input/output account states, instruction and CPI boundaries, syscall call rows, SBPF pre-instruction registers/opcodes, aggregate compute, invocation memory before/after deltas, errors, and commit/rollback outcome. It intentionally does not record host timestamps, pointers, JIT machine code, cache hits, metrics, or log transport.

Ordinary SBPF load/store transitions can be reconstructed from opcode, registers, and invocation memory. Trace v1 records memory snapshots/deltas at invocation boundaries rather than adding a callback to the external `solana-sbpf` crate. A later schema version may add online per-access events without changing proof kind `2` public-input order.

## Initial conformance corpus

The frozen corpus contains these semantic cases:

1. successful SBF account write and return data;
2. failed SBF execution with rollback after a write;
3. invalid transaction signature;
4. expired/unknown recent blockhash;
5. fee-payer or account-load failure with correct fee/rollback behavior;
6. System Program transfer;
7. Token-2022 transfer;
8. successful SBF-to-System CPI.

Each fixture stores canonical input, expected Agave result, expected trace hash, trace byte length, event/register-row counts, executed units, fee details, and final/rollback account commitments. Replaying one fixture twice from identical snapshots must produce identical bytes and hash.
