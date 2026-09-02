use {
    log::*,
    northstar_portal::{
        find_checkpoint_cursor_pda, find_checkpoint_pda,
        find_delegation_record_pda as find_portal_delegation_record_pda, Checkpoint,
        CheckpointStatus, SettlementStatus, MAX_CHALLENGE_WINDOW_SLOTS,
    },
    portal_state::{try_parse_raw_portal_account, PortalAccount},
    solana_account::{AccountSharedData, ReadableAccount},
    solana_accounts_db::accounts_index::IndexKey,
    solana_fee_structure::FeeStructure,
    solana_gossip::cluster_info::ClusterInfo,
    solana_hash::Hash,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_pubkey::Pubkey,
    solana_rpc::er_history::DEFAULT_MAX_RETAINED_SLOTS,
    solana_runtime::bank::Bank,
    solana_sdk_ids::{system_program, sysvar},
    solana_signer::Signer,
    solana_transaction::Transaction,
    std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration},
    thiserror::Error,
    unsettled_state::{RecoveredUnsettledState, RecoveryDisposition, UnsettledSessionIdentity},
};

pub mod ephemeral_runtime;
pub mod ephemeral_tpu;
pub mod ephemeral_tx_client;
pub mod portal_state;
pub mod settlement;
pub mod slot_advancer;
pub mod unsettled_state;

pub use crate::{
    ephemeral_runtime::{EphemeralRuntime, ErStateDiff, ErStateDiffAccount},
    settlement::{build_settlement_plan, SettlementPlan},
};

const DEFAULT_ER_SLOT_DURATION_MS: u64 = 50;
const DEFAULT_CHECKPOINT_CHALLENGE_WINDOW_SLOTS: u64 = 10;
pub const DEFAULT_ER_SLOT_DURATION: Duration = Duration::from_millis(DEFAULT_ER_SLOT_DURATION_MS);
pub(crate) const DEFAULT_ER_TRANSACTION_MAX_AGE: usize = (solana_clock::MAX_PROCESSING_AGE
    * solana_clock::DEFAULT_MS_PER_SLOT as usize)
    .div_ceil(DEFAULT_ER_SLOT_DURATION_MS as usize);
pub(crate) fn er_transaction_max_age_for_slot_duration(slot_duration: Duration) -> usize {
    scale_l1_slot_age_for_er(solana_clock::MAX_PROCESSING_AGE, slot_duration)
}

pub(crate) fn er_recent_blockhash_max_age_for_slot_duration(slot_duration: Duration) -> usize {
    scale_l1_slot_age_for_er(solana_clock::MAX_RECENT_BLOCKHASHES, slot_duration)
}

fn scale_l1_slot_age_for_er(l1_slot_age: usize, slot_duration: Duration) -> usize {
    let er_slot_ms = usize::try_from(slot_duration.as_millis())
        .unwrap_or(usize::MAX)
        .max(1);
    (l1_slot_age * solana_clock::DEFAULT_MS_PER_SLOT as usize).div_ceil(er_slot_ms)
}

fn deposit_receipt_escrow_lamports(bank: &Bank, lamports: u64, data_len: usize) -> u64 {
    lamports.saturating_sub(bank.get_minimum_balance_for_rent_exemption(data_len))
}

/// Fixed ER account that receives all bridged SOL withdrawals.
pub const WITHDRAWAL_SINK: Pubkey = northstar_portal::WITHDRAWAL_SINK;

/// Build the ER Portal instruction for a bridged SOL withdrawal request.
pub fn er_withdrawal_instruction(
    portal_program_id: &Pubkey,
    source: &Pubkey,
    l1_recipient: &Pubkey,
    lamports: u64,
) -> Instruction {
    let mut data = [0; 9];
    data[0] = 13;
    data[1..].copy_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: *portal_program_id,
        accounts: vec![
            AccountMeta::new(*source, true),
            AccountMeta::new_readonly(*l1_recipient, false),
            AccountMeta::new(WITHDRAWAL_SINK, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(sysvar::clock::id(), false),
        ],
        data: data.to_vec(),
    }
}

#[derive(Error, Debug)]
pub enum NorthStarError {
    #[error("Failed to create ephemeral runtime: {0}")]
    RuntimeCreationFailed(String),
}

pub type Result<T> = std::result::Result<T, NorthStarError>;

#[derive(Debug, Clone)]
pub struct EphemeralRollupSettings {
    pub session_pda: Pubkey,
    pub grid_id: u64,
    pub ttl_slots: u64,
    /// Explicit ER transaction fee model.
    ///
    /// `Self::zero_fee_structure()` enables demo/devnet gasless ER
    /// transactions while keeping non-zero fee configs available for future
    /// examples.
    pub er_fee_structure: FeeStructure,
    pub fee_cap: u64,
    pub delegated_accounts: Vec<Pubkey>,
}

impl EphemeralRollupSettings {
    /// Explicit zero-fee ER model for demos/devnet.
    ///
    /// Keep every fee component at zero, not only signature fees. In
    /// particular, this must not inherit `FeeStructure::default()` because its
    /// defaults are L1-oriented and may grow more non-zero components over time.
    pub fn zero_fee_structure() -> FeeStructure {
        FeeStructure {
            lamports_per_signature: 0,
            lamports_per_write_lock: 0,
            compute_fee_bins: vec![],
        }
    }
}

/// Events detected on L1 that are relevant to ephemeral rollups.
///
/// These events are emitted when the NorthStar service scans portal
/// program accounts and detects state changes.
#[derive(Debug, Clone)]
pub enum L1Event {
    /// A new Session PDA was created on L1
    SessionOpened {
        session_pda: Pubkey,
        grid_id: u64,
        ttl_slots: u64,
        fee_cap: u64,
    },
    /// A Session PDA was closed on L1
    SessionClosed { session_pda: Pubkey, grid_id: u64 },
    /// An account was delegated to the portal program
    AccountDelegated {
        delegation_record_pda: Pubkey,
        delegated_account: Pubkey,
        owner_program: Pubkey,
        grid_id: u64,
    },
    /// An account was undelegated (returned to original owner)
    AccountUndelegated {
        delegation_record_pda: Pubkey,
        delegated_account: Pubkey,
    },
    /// A fee deposit was made
    FeeDeposited {
        session_pda: Pubkey,
        /// Total escrowed lamports after the deposit
        amount: u64,
        /// Deposit amount this slot (current escrow - parent escrow)
        delta: u64,
        /// Who gets credited on L2
        depositor: Pubkey,
    },
    /// SPL tokens locked on L1 for a delegated ER token account.
    TokenDeposited {
        session_pda: Pubkey,
        session_bridge: Pubkey,
        bridge_program: Pubkey,
        er_token_account: Pubkey,
        amount: u64,
        delta: u64,
    },
}

/// Configuration for NorthStar Manager
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    /// Portal program ID (to read L1 state from)
    pub portal_program_id: Pubkey,

    /// Manager account keypair (for signing transactions in ephemeral rollups)
    pub manager_account: Arc<Keypair>,

    /// Durable directory for checkpoint-bound settlement plan snapshots.
    pub checkpoint_plan_dir: Option<PathBuf>,
}

/// Metadata about an ephemeral fork
#[derive(Debug, Clone)]
pub struct EphemeralForkMetadata {}

#[derive(borsh::BorshDeserialize, borsh::BorshSerialize)]
struct DurableSettlementPlan {
    er_slot: u64,
    checksum: [u8; 32],
    chunks: Vec<DurableSettlementChunk>,
    owner_changes: Vec<DurableAccountOwnerSettlement>,
    lamport_changes: Vec<DurableAccountLamportsSettlement>,
    receipt_balances: Vec<DurableReceiptBalanceSettlement>,
    token_withdrawals: Vec<DurableTokenWithdrawalSettlement>,
}

#[derive(borsh::BorshDeserialize, borsh::BorshSerialize)]
struct DurableSettlementChunk {
    account: [u8; 32],
    account_data_offset: u32,
    data: Vec<u8>,
}

#[derive(borsh::BorshDeserialize, borsh::BorshSerialize)]
struct DurableAccountOwnerSettlement {
    account: [u8; 32],
    owner: [u8; 32],
}

#[derive(borsh::BorshDeserialize, borsh::BorshSerialize)]
struct DurableAccountLamportsSettlement {
    account: [u8; 32],
    lamports: u64,
}

#[derive(borsh::BorshDeserialize, borsh::BorshSerialize)]
struct DurableReceiptBalanceSettlement {
    er_source: [u8; 32],
    l1_recipient: [u8; 32],
    balance: u64,
    withdrawn: u64,
    payout_lamports: u64,
}

#[derive(borsh::BorshDeserialize, borsh::BorshSerialize)]
struct DurableTokenWithdrawalSettlement {
    bridge_program: [u8; 32],
    session_bridge: [u8; 32],
    er_token_account: [u8; 32],
    vault: [u8; 32],
    vault_token_account: [u8; 32],
    l1_destination_token_account: [u8; 32],
    mint: [u8; 32],
    token_program: [u8; 32],
    amount: u64,
    withdrawn: u64,
    decimals: u8,
}

impl From<&SettlementPlan> for DurableSettlementPlan {
    fn from(plan: &SettlementPlan) -> Self {
        Self {
            er_slot: plan.er_slot,
            checksum: plan.checksum,
            chunks: plan
                .chunks
                .iter()
                .map(|chunk| DurableSettlementChunk {
                    account: chunk.account.to_bytes(),
                    account_data_offset: chunk.account_data_offset,
                    data: chunk.data.clone(),
                })
                .collect(),
            owner_changes: plan
                .owner_changes
                .iter()
                .map(|change| DurableAccountOwnerSettlement {
                    account: change.account.to_bytes(),
                    owner: change.owner.to_bytes(),
                })
                .collect(),
            lamport_changes: plan
                .lamport_changes
                .iter()
                .map(|change| DurableAccountLamportsSettlement {
                    account: change.account.to_bytes(),
                    lamports: change.lamports,
                })
                .collect(),
            receipt_balances: plan
                .receipt_balances
                .iter()
                .map(|receipt| DurableReceiptBalanceSettlement {
                    er_source: receipt.er_source.to_bytes(),
                    l1_recipient: receipt.l1_recipient.to_bytes(),
                    balance: receipt.balance,
                    withdrawn: receipt.withdrawn,
                    payout_lamports: receipt.payout_lamports,
                })
                .collect(),
            token_withdrawals: plan
                .token_withdrawals
                .iter()
                .map(|withdrawal| DurableTokenWithdrawalSettlement {
                    bridge_program: withdrawal.bridge_program.to_bytes(),
                    session_bridge: withdrawal.session_bridge.to_bytes(),
                    er_token_account: withdrawal.er_token_account.to_bytes(),
                    vault: withdrawal.vault.to_bytes(),
                    vault_token_account: withdrawal.vault_token_account.to_bytes(),
                    l1_destination_token_account: withdrawal
                        .l1_destination_token_account
                        .to_bytes(),
                    mint: withdrawal.mint.to_bytes(),
                    token_program: withdrawal.token_program.to_bytes(),
                    amount: withdrawal.amount,
                    withdrawn: withdrawal.withdrawn,
                    decimals: withdrawal.decimals,
                })
                .collect(),
        }
    }
}

impl From<DurableSettlementPlan> for SettlementPlan {
    fn from(plan: DurableSettlementPlan) -> Self {
        Self {
            er_slot: plan.er_slot,
            checksum: plan.checksum,
            chunks: plan
                .chunks
                .into_iter()
                .map(|chunk| settlement::SettlementChunk {
                    account: Pubkey::new_from_array(chunk.account),
                    account_data_offset: chunk.account_data_offset,
                    data: chunk.data,
                })
                .collect(),
            owner_changes: plan
                .owner_changes
                .into_iter()
                .map(|change| settlement::AccountOwnerSettlement {
                    account: Pubkey::new_from_array(change.account),
                    owner: Pubkey::new_from_array(change.owner),
                })
                .collect(),
            lamport_changes: plan
                .lamport_changes
                .into_iter()
                .map(|change| settlement::AccountLamportsSettlement {
                    account: Pubkey::new_from_array(change.account),
                    lamports: change.lamports,
                })
                .collect(),
            receipt_balances: plan
                .receipt_balances
                .into_iter()
                .map(|receipt| settlement::ReceiptBalanceSettlement {
                    er_source: Pubkey::new_from_array(receipt.er_source),
                    l1_recipient: Pubkey::new_from_array(receipt.l1_recipient),
                    balance: receipt.balance,
                    withdrawn: receipt.withdrawn,
                    payout_lamports: receipt.payout_lamports,
                })
                .collect(),
            token_withdrawals: plan
                .token_withdrawals
                .into_iter()
                .map(|withdrawal| settlement::TokenWithdrawalSettlement {
                    bridge_program: Pubkey::new_from_array(withdrawal.bridge_program),
                    session_bridge: Pubkey::new_from_array(withdrawal.session_bridge),
                    er_token_account: Pubkey::new_from_array(withdrawal.er_token_account),
                    vault: Pubkey::new_from_array(withdrawal.vault),
                    vault_token_account: Pubkey::new_from_array(withdrawal.vault_token_account),
                    l1_destination_token_account: Pubkey::new_from_array(
                        withdrawal.l1_destination_token_account,
                    ),
                    mint: Pubkey::new_from_array(withdrawal.mint),
                    token_program: Pubkey::new_from_array(withdrawal.token_program),
                    amount: withdrawal.amount,
                    withdrawn: withdrawal.withdrawn,
                    decimals: withdrawal.decimals,
                })
                .collect(),
            unsupported_changes: vec![],
        }
    }
}

/// Main manager for ephemeral rollup forks
pub struct Manager {
    config: ManagerConfig,
    slot_duration: Duration,
    er_history_max_retained_slots: usize,
    checkpoint_plans: std::sync::RwLock<HashMap<(Pubkey, u64), SettlementPlan>>,
    /// Sonic: Always-on ephemeral runtime. Created once at startup via
    /// `init_runtime()`, stays alive for the validator's lifetime.
    /// The `active` flag inside gates transaction acceptance.
    runtime: Option<EphemeralRuntime>,
}

impl Manager {
    /// Create a new NorthStar Manager
    pub fn new(config: ManagerConfig) -> Self {
        info!("Initializing NorthStar Manager with config: {config:?}");
        Self {
            config,
            slot_duration: DEFAULT_ER_SLOT_DURATION,
            er_history_max_retained_slots: DEFAULT_MAX_RETAINED_SLOTS,
            checkpoint_plans: std::sync::RwLock::new(HashMap::new()),
            runtime: None,
        }
    }

    pub fn set_slot_duration(&mut self, slot_duration: Duration) {
        self.slot_duration = slot_duration;
    }

    pub fn set_er_history_max_retained_slots(&mut self, max_retained_slots: usize) {
        self.er_history_max_retained_slots = max_retained_slots;
    }

    /// Sonic: Check if an ephemeral session is currently active (accepting transactions)
    pub fn has_active_runtime(&self) -> bool {
        self.runtime.as_ref().is_some_and(|r| r.is_active())
    }

    /// Sonic: Check if the always-on runtime has been initialized
    pub fn has_runtime(&self) -> bool {
        self.runtime.is_some()
    }

    /// Sonic: Update latest L1 slot observed by the NorthStar sync loop.
    pub fn update_latest_l1_slot(&self, slot: u64) {
        if let Some(runtime) = &self.runtime {
            runtime.update_latest_l1_slot(slot);
        }
    }

    /// Sonic: Mark L1 events synced through `slot`.
    pub fn mark_synced_through(&self, slot: u64) {
        if let Some(runtime) = &self.runtime {
            runtime.mark_synced_through(slot);
        }
    }

    /// Get the RPC address of the runtime, if initialized
    pub fn runtime_addr(&self) -> Option<String> {
        self.runtime.as_ref().map(|r| r.rpc_addr())
    }

    /// Get the WebSocket address of the runtime, if initialized
    pub fn runtime_ws_addr(&self) -> Option<String> {
        self.runtime.as_ref().map(|r| r.ws_addr())
    }

    /// Get the session PDA Arc, if runtime initialized
    pub fn session_pda(&self) -> Option<Arc<std::sync::RwLock<Option<Pubkey>>>> {
        self.runtime.as_ref().map(|r| r.session_pda())
    }

    /// Compute current ER-vs-L1 state diff/hash, if runtime exists.
    pub fn state_diff_from_l1(&self) -> Option<ErStateDiff> {
        self.runtime
            .as_ref()
            .map(|runtime| runtime.state_diff_from_l1())
    }

    /// Build data-only delegated account settlement chunks.
    ///
    /// Returns `None` when there are no data changes to settle, so devnet
    /// testing does not burn SOL on empty Begin/Finish transactions.
    pub fn settlement_plan(&self) -> Option<SettlementPlan> {
        let runtime = self.runtime.as_ref()?;
        let session_pda = (*runtime.session_pda().read().unwrap())?;
        let diff = runtime.state_diff_from_l1();
        let er_slot = runtime.bank().slot();
        let receipt_balances = runtime.settlement_receipt_balances(session_pda);
        let token_withdrawals = runtime.settlement_token_withdrawals(er_slot);
        let mut plan = build_settlement_plan(
            &diff,
            &runtime.delegated_accounts(),
            er_slot,
            receipt_balances,
        )
        .or_else(|| {
            (!token_withdrawals.is_empty()).then(|| SettlementPlan {
                er_slot,
                checksum: [0; 32],
                chunks: vec![],
                owner_changes: vec![],
                lamport_changes: vec![],
                receipt_balances: vec![],
                token_withdrawals: vec![],
                unsupported_changes: vec![],
            })
        })?;
        plan.token_withdrawals = token_withdrawals;
        plan.checksum = plan.recomputed_checksum();
        Some(plan)
    }

    /// Build Portal settlement instructions for current data-only diff.
    ///
    /// Returns `None` when there is no diff to submit. Callers should treat
    /// that as a hard no-op and must not send empty Begin/Finish transactions.
    pub fn settlement_instructions(&self) -> Option<Vec<Instruction>> {
        let runtime = self.runtime.as_ref()?;
        let session_pda = (*runtime.session_pda().read().unwrap())?;
        let plan = self.settlement_plan()?;
        let instructions = plan.portal_instructions(
            self.config.portal_program_id,
            session_pda,
            self.config.manager_account.pubkey(),
        );
        (!instructions.is_empty()).then_some(instructions)
    }

    /// Re-sign a queued settlement transaction with a fresh L1 blockhash before
    /// submitting it. Split settlements are submitted one transaction at a time,
    /// so later transactions must not keep the stale blockhash used when the
    /// original plan was built.
    pub fn resign_settlement_transaction(
        &self,
        transaction: &mut Transaction,
        recent_blockhash: Hash,
    ) {
        transaction.sign(&[self.config.manager_account.as_ref()], recent_blockhash);
    }

    /// Build signed Portal checkpoint/settlement transactions if the L1 Session
    /// interval has elapsed and a non-empty diff exists.
    pub fn settlement_transactions_if_due(
        &self,
        l1_bank: &Bank,
        recent_blockhash: Hash,
    ) -> Option<(u64, [u8; 32], Vec<Transaction>)> {
        let runtime = self.runtime.as_ref()?;
        let session_pda = (*runtime.session_pda().read().unwrap())?;
        let session_account = l1_bank.get_account(&session_pda)?;
        if session_account.owner() != &self.config.portal_program_id {
            return None;
        }
        let PortalAccount::Session(session_state) =
            try_parse_raw_portal_account(session_account.data())?
        else {
            return None;
        };
        if !session_state.is_valid() {
            return None;
        }
        self.cleanup_terminal_checkpoint_plans(l1_bank, session_pda);
        if let Some(plan) = self.pending_token_release_plan(l1_bank, session_pda) {
            let transactions = settlement::token_withdrawal_transactions(
                &plan.token_withdrawals,
                self.config.portal_program_id,
                session_pda,
                plan.er_slot,
                plan.checksum,
                self.config.manager_account.as_ref(),
                recent_blockhash,
            );
            return (!transactions.is_empty()).then_some((
                plan.er_slot,
                plan.checksum,
                transactions,
            ));
        }

        let (plan, transactions) = match session_state.settlement_status {
            SettlementStatus::Idle => {
                let next_settlement_slot = session_state
                    .last_settled_l1_slot
                    .saturating_add(session_state.settlement_interval_slots);
                if l1_bank.slot() < next_settlement_slot {
                    return None;
                }
                if let Some((checkpoint_pda, checkpoint)) =
                    self.active_checkpoint_for_session(l1_bank, session_pda)
                {
                    let plan = self.checkpoint_bound_plan(session_pda, &checkpoint)?;
                    let transactions = self.transactions_for_existing_checkpoint(
                        l1_bank,
                        session_pda,
                        &plan,
                        checkpoint_pda,
                        checkpoint,
                        recent_blockhash,
                    )?;
                    (plan, transactions)
                } else {
                    let plan = self.settlement_plan()?;
                    let transactions = self.checkpoint_or_settlement_transactions(
                        l1_bank,
                        session_pda,
                        &plan,
                        session_state.settlement_interval_slots,
                        recent_blockhash,
                    )?;
                    (plan, transactions)
                }
            }
            SettlementStatus::InProgress => {
                let plan = self
                    .active_checkpoint_for_session(l1_bank, session_pda)
                    .and_then(|(_, checkpoint)| {
                        self.checkpoint_bound_plan(session_pda, &checkpoint)
                    })
                    .or_else(|| {
                        self.cached_checkpoint_plan(session_pda, session_state.settlement_er_slot)
                    })
                    .or_else(|| self.settlement_plan())?;
                if session_state.settlement_er_slot != plan.er_slot
                    || session_state.settlement_checksum != plan.checksum
                {
                    warn!(
                        "Portal settlement retry blocked: checkpoint plan mismatch for er_slot={} \
                         session_er_slot={} checksum={:?} session_checksum={:?}",
                        plan.er_slot,
                        session_state.settlement_er_slot,
                        plan.checksum,
                        session_state.settlement_checksum,
                    );
                    return None;
                }
                let transactions = self.settlement_transactions_for_plan(
                    &plan,
                    session_pda,
                    recent_blockhash,
                    false,
                );
                (plan, transactions)
            }
        };
        (!transactions.is_empty()).then_some((plan.er_slot, plan.checksum, transactions))
    }

    fn checkpoint_plan_dir(&self) -> PathBuf {
        if let Some(path) = &self.config.checkpoint_plan_dir {
            return path.clone();
        }
        if let Some(path) = std::env::var_os("NORTHSTAR_CHECKPOINT_PLAN_DIR").map(PathBuf::from) {
            return path;
        }
        warn!(
            "NORTHSTAR_CHECKPOINT_PLAN_DIR is unset; using current-directory checkpoint plan store"
        );
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("northstar-checkpoint-plans")
    }

    fn unsettled_state_dir(&self) -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("NORTHSTAR_UNSETTLED_STATE_DIR").map(PathBuf::from) {
            return Some(path);
        }
        let checkpoint_dir = self
            .config
            .checkpoint_plan_dir
            .clone()
            .or_else(|| std::env::var_os("NORTHSTAR_CHECKPOINT_PLAN_DIR").map(PathBuf::from))?;
        Some(
            checkpoint_dir
                .parent()
                .unwrap_or(&checkpoint_dir)
                .join("northstar-unsettled-state"),
        )
    }

    fn checkpoint_plan_path(&self, session_pda: Pubkey, proposer: Pubkey, er_slot: u64) -> PathBuf {
        self.checkpoint_plan_dir().join(format!(
            "{}-{}-{}-{}.borsh",
            self.config.portal_program_id, session_pda, proposer, er_slot
        ))
    }

    fn persist_checkpoint_plan(&self, session_pda: Pubkey, plan: &SettlementPlan) {
        let path = self.checkpoint_plan_path(
            session_pda,
            self.config.manager_account.pubkey(),
            plan.er_slot,
        );
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                warn!("Failed to create checkpoint plan dir {parent:?}: {err}");
                return;
            }
        }
        match borsh::to_vec(&DurableSettlementPlan::from(plan)) {
            Ok(bytes) => {
                let tmp_path = path.with_extension(format!("borsh.tmp.{}", std::process::id()));
                let write_result = std::fs::File::create(&tmp_path).and_then(|mut file| {
                    std::io::Write::write_all(&mut file, &bytes)?;
                    file.sync_all()
                });
                if let Err(err) = write_result {
                    warn!("Failed to persist checkpoint plan {tmp_path:?}: {err}");
                    let _ = std::fs::remove_file(&tmp_path);
                    return;
                }
                if let Err(err) = std::fs::rename(&tmp_path, &path) {
                    warn!("Failed to install checkpoint plan {path:?}: {err}");
                    let _ = std::fs::remove_file(&tmp_path);
                }
            }
            Err(err) => warn!("Failed to serialize checkpoint plan: {err}"),
        }
    }

    fn load_persisted_checkpoint_plan(
        &self,
        session_pda: Pubkey,
        checkpoint: &Checkpoint,
    ) -> Option<SettlementPlan> {
        let proposer = checkpoint.proposer;
        let path = self.checkpoint_plan_path(session_pda, proposer, checkpoint.er_slot);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                warn!("Missing checkpoint plan {path:?}: {err}");
                return None;
            }
        };
        let durable = match borsh::from_slice::<DurableSettlementPlan>(&bytes) {
            Ok(plan) => plan,
            Err(err) => {
                warn!("Failed to deserialize checkpoint plan {path:?}: {err}");
                let _ = std::fs::remove_file(&path);
                return None;
            }
        };
        let plan: SettlementPlan = durable.into();
        let recomputed_checksum = plan.recomputed_checksum();
        if recomputed_checksum != plan.checksum
            || recomputed_checksum != checkpoint.effect_commitment
        {
            warn!(
                "Checkpoint plan checksum mismatch for {path:?}: stored={:?} recomputed={:?} \
                 checkpoint={:?}",
                plan.checksum, recomputed_checksum, checkpoint.effect_commitment,
            );
            let _ = std::fs::remove_file(&path);
            return None;
        }
        Some(plan)
    }

    fn cache_checkpoint_plan(&self, session_pda: Pubkey, plan: &SettlementPlan) {
        self.checkpoint_plans
            .write()
            .unwrap()
            .insert((session_pda, plan.er_slot), plan.clone());
        self.persist_checkpoint_plan(session_pda, plan);
    }

    fn cached_checkpoint_plan(&self, session_pda: Pubkey, er_slot: u64) -> Option<SettlementPlan> {
        self.checkpoint_plans
            .read()
            .unwrap()
            .get(&(session_pda, er_slot))
            .cloned()
    }

    fn remove_checkpoint_plan(&self, session_pda: Pubkey, checkpoint: &Checkpoint) {
        self.checkpoint_plans
            .write()
            .unwrap()
            .remove(&(session_pda, checkpoint.er_slot));
        let proposer = checkpoint.proposer;
        let path = self.checkpoint_plan_path(session_pda, proposer, checkpoint.er_slot);
        if let Err(err) = std::fs::remove_file(&path) {
            if err.kind() != std::io::ErrorKind::NotFound {
                warn!("Failed to remove checkpoint plan {path:?}: {err}");
            }
        }
    }

    fn token_withdrawals_complete(l1_bank: &Bank, plan: &SettlementPlan) -> bool {
        plan.token_withdrawals.iter().all(|withdrawal| {
            l1_bank
                .get_account(&withdrawal.vault)
                .filter(|account| account.owner() == &withdrawal.bridge_program)
                .and_then(|account| {
                    borsh::from_slice::<northstar_token_bridge::state::TokenVault>(account.data())
                        .ok()
                })
                .is_some_and(|vault| vault.is_valid() && vault.withdrawn >= withdrawal.withdrawn)
        })
    }

    fn pending_token_release_plan(
        &self,
        l1_bank: &Bank,
        session_pda: Pubkey,
    ) -> Option<SettlementPlan> {
        let plans = self
            .checkpoint_plans
            .read()
            .unwrap()
            .iter()
            .filter_map(|((plan_session, _), plan)| {
                (plan_session == &session_pda).then_some(plan.clone())
            })
            .collect::<Vec<_>>();

        plans.into_iter().find(|plan| {
            if plan.token_withdrawals.is_empty() || Self::token_withdrawals_complete(l1_bank, plan)
            {
                return false;
            }
            let checkpoint =
                find_checkpoint_pda(&self.config.portal_program_id, &session_pda, plan.er_slot).0;
            l1_bank
                .get_account(&checkpoint)
                .filter(|account| account.owner() == &self.config.portal_program_id)
                .and_then(|account| try_parse_raw_portal_account(account.data()))
                .is_some_and(|account| {
                    matches!(
                        account,
                        PortalAccount::Checkpoint(checkpoint)
                            if checkpoint.is_valid()
                                && checkpoint.session == session_pda
                                && checkpoint.er_slot == plan.er_slot
                                && checkpoint.effect_commitment == plan.checksum
                                && checkpoint.status == CheckpointStatus::Settled
                    )
                })
        })
    }

    fn cleanup_terminal_checkpoint_plans(&self, l1_bank: &Bank, session_pda: Pubkey) {
        // IMPORTANT: `get_program_accounts` falls back to a global AccountsIndex scan when the
        // validator has no program-id secondary index. Settlement runs on every frozen bank, so
        // only inspect checkpoint PDAs already tracked by this manager.
        let er_slots = self
            .checkpoint_plans
            .read()
            .unwrap()
            .keys()
            .filter_map(|(plan_session, er_slot)| {
                (plan_session == &session_pda).then_some(*er_slot)
            })
            .collect::<Vec<_>>();

        for er_slot in er_slots {
            let (checkpoint_pda, _) =
                find_checkpoint_pda(&self.config.portal_program_id, &session_pda, er_slot);
            let Some(account) = l1_bank.get_account(&checkpoint_pda) else {
                continue;
            };
            let Some(PortalAccount::Checkpoint(checkpoint)) =
                try_parse_raw_portal_account(account.data())
            else {
                continue;
            };
            let can_remove = matches!(
                checkpoint.status,
                CheckpointStatus::Cancelled | CheckpointStatus::Invalid
            ) || (checkpoint.status == CheckpointStatus::Settled
                && self
                    .cached_checkpoint_plan(session_pda, checkpoint.er_slot)
                    .is_none_or(|plan| Self::token_withdrawals_complete(l1_bank, &plan)));
            if checkpoint.session == session_pda && can_remove {
                self.remove_checkpoint_plan(session_pda, &checkpoint);
            }
        }
    }

    fn checkpoint_bound_plan(
        &self,
        session_pda: Pubkey,
        checkpoint: &Checkpoint,
    ) -> Option<SettlementPlan> {
        let Some(plan) = self
            .cached_checkpoint_plan(session_pda, checkpoint.er_slot)
            .or_else(|| self.load_persisted_checkpoint_plan(session_pda, checkpoint))
        else {
            warn!(
                "Portal active checkpoint has no durable settlement plan: er_slot={} checksum={:?}",
                checkpoint.er_slot, checkpoint.effect_commitment,
            );
            return None;
        };
        if plan.checksum != checkpoint.effect_commitment {
            warn!(
                "Portal cached checkpoint plan mismatch: er_slot={} plan_checksum={:?} \
                 checkpoint_effect={:?}",
                checkpoint.er_slot, plan.checksum, checkpoint.effect_commitment,
            );
            return None;
        }
        self.checkpoint_plans
            .write()
            .unwrap()
            .insert((session_pda, plan.er_slot), plan.clone());
        Some(plan)
    }

    fn checkpoint_or_settlement_transactions(
        &self,
        l1_bank: &Bank,
        session_pda: Pubkey,
        plan: &SettlementPlan,
        challenge_window_slots: u64,
        recent_blockhash: Hash,
    ) -> Option<Vec<Transaction>> {
        let challenge_window_slots = challenge_window_slots.clamp(
            DEFAULT_CHECKPOINT_CHALLENGE_WINDOW_SLOTS,
            MAX_CHALLENGE_WINDOW_SLOTS,
        );
        if let Some((checkpoint_pda, checkpoint)) =
            self.active_checkpoint_for_session(l1_bank, session_pda)
        {
            return self.transactions_for_existing_checkpoint(
                l1_bank,
                session_pda,
                plan,
                checkpoint_pda,
                checkpoint,
                recent_blockhash,
            );
        }

        let (checkpoint_pda, _) =
            find_checkpoint_pda(&self.config.portal_program_id, &session_pda, plan.er_slot);
        let Some(checkpoint_account) = l1_bank.get_account(&checkpoint_pda) else {
            info!(
                "Portal checkpoint propose: er_slot={} checksum={:?} challenge_window_slots={}",
                plan.er_slot, plan.checksum, challenge_window_slots,
            );
            let transaction = plan.checkpoint_proposal_transaction(
                self.config.portal_program_id,
                session_pda,
                self.config.manager_account.as_ref(),
                recent_blockhash,
                challenge_window_slots,
                self.latest_finalized_checkpoint_state_root(l1_bank, session_pda),
            )?;
            self.cache_checkpoint_plan(session_pda, plan);
            return Some(vec![transaction]);
        };
        if checkpoint_account.owner() != &self.config.portal_program_id {
            warn!("Portal checkpoint {checkpoint_pda} has wrong owner");
            return None;
        }
        let PortalAccount::Checkpoint(checkpoint) =
            try_parse_raw_portal_account(checkpoint_account.data())?
        else {
            warn!("Portal checkpoint {checkpoint_pda} has invalid account data");
            return None;
        };
        if matches!(
            checkpoint.status,
            CheckpointStatus::Settled | CheckpointStatus::Cancelled | CheckpointStatus::Invalid
        ) {
            let previous_state_root =
                self.latest_finalized_checkpoint_state_root(l1_bank, session_pda);
            info!(
                "Portal checkpoint re-propose over terminal account: er_slot={} checksum={:?}",
                plan.er_slot, plan.checksum,
            );
            let transaction = plan.checkpoint_proposal_transaction(
                self.config.portal_program_id,
                session_pda,
                self.config.manager_account.as_ref(),
                recent_blockhash,
                challenge_window_slots,
                previous_state_root,
            )?;
            self.cache_checkpoint_plan(session_pda, plan);
            return Some(vec![transaction]);
        }
        self.transactions_for_existing_checkpoint(
            l1_bank,
            session_pda,
            plan,
            checkpoint_pda,
            checkpoint,
            recent_blockhash,
        )
    }

    fn latest_finalized_checkpoint_state_root(
        &self,
        l1_bank: &Bank,
        session_pda: Pubkey,
    ) -> [u8; 32] {
        let (cursor_pda, _) =
            find_checkpoint_cursor_pda(&self.config.portal_program_id, &session_pda);
        let Some(cursor_account) = l1_bank.get_account(&cursor_pda) else {
            return [0; 32];
        };
        let Some(PortalAccount::CheckpointCursor(cursor)) =
            try_parse_raw_portal_account(cursor_account.data())
        else {
            warn!("Portal checkpoint cursor {cursor_pda} has invalid account data");
            return [0; 32];
        };
        if cursor.session != session_pda {
            warn!("Portal checkpoint cursor {cursor_pda} session mismatch");
            return [0; 32];
        }
        cursor.latest_finalized_state_root
    }

    fn active_checkpoint_for_session(
        &self,
        l1_bank: &Bank,
        session_pda: Pubkey,
    ) -> Option<(Pubkey, Checkpoint)> {
        // The cursor is the on-chain index for the one active checkpoint. Never rediscover it by
        // scanning every Portal-owned account on the settlement hot path.
        let (cursor_pda, _) =
            find_checkpoint_cursor_pda(&self.config.portal_program_id, &session_pda);
        let cursor_account = l1_bank.get_account(&cursor_pda)?;
        let PortalAccount::CheckpointCursor(cursor) =
            try_parse_raw_portal_account(cursor_account.data())?
        else {
            return None;
        };
        if !cursor.is_valid()
            || cursor.session != session_pda
            || cursor.active_checkpoint == Pubkey::default()
        {
            return None;
        }

        let checkpoint_pda = cursor.active_checkpoint;
        let checkpoint_account = l1_bank.get_account(&checkpoint_pda)?;
        if checkpoint_account.owner() != &self.config.portal_program_id {
            return None;
        }
        let PortalAccount::Checkpoint(checkpoint) =
            try_parse_raw_portal_account(checkpoint_account.data())?
        else {
            return None;
        };
        (checkpoint.is_valid()
            && checkpoint.session == session_pda
            && matches!(
                checkpoint.status,
                CheckpointStatus::Pending
                    | CheckpointStatus::Committed
                    | CheckpointStatus::Challenged
            ))
        .then_some((checkpoint_pda, checkpoint))
    }

    fn settlement_transactions_for_plan(
        &self,
        plan: &SettlementPlan,
        session_pda: Pubkey,
        recent_blockhash: Hash,
        include_begin: bool,
    ) -> Vec<Transaction> {
        let mut transactions = if include_begin {
            plan.portal_transactions(
                self.config.portal_program_id,
                session_pda,
                self.config.manager_account.as_ref(),
                recent_blockhash,
            )
        } else {
            plan.portal_retry_transactions_after_begin(
                self.config.portal_program_id,
                session_pda,
                self.config.manager_account.as_ref(),
                recent_blockhash,
            )
        };
        let Some(finish_transaction) = transactions.pop() else {
            return vec![];
        };
        transactions.extend(settlement::token_withdrawal_authorization_transactions(
            &plan.token_withdrawals,
            self.config.portal_program_id,
            session_pda,
            plan.er_slot,
            plan.checksum,
            self.config.manager_account.as_ref(),
            recent_blockhash,
        ));
        transactions.push(finish_transaction);
        transactions.extend(settlement::token_withdrawal_transactions(
            &plan.token_withdrawals,
            self.config.portal_program_id,
            session_pda,
            plan.er_slot,
            plan.checksum,
            self.config.manager_account.as_ref(),
            recent_blockhash,
        ));
        transactions
    }

    fn transactions_for_existing_checkpoint(
        &self,
        l1_bank: &Bank,
        session_pda: Pubkey,
        plan: &SettlementPlan,
        checkpoint_pda: Pubkey,
        checkpoint: Checkpoint,
        recent_blockhash: Hash,
    ) -> Option<Vec<Transaction>> {
        if !checkpoint.is_valid()
            || checkpoint.session != session_pda
            || checkpoint.er_slot != plan.er_slot
        {
            warn!(
                "Portal checkpoint mismatch: checkpoint={} plan_er_slot={}",
                checkpoint_pda, plan.er_slot,
            );
            return None;
        }
        if checkpoint.effect_commitment != plan.checksum {
            warn!(
                "Portal checkpoint/live diff mismatch for er_slot={}: checkpoint_effect={:?} \
                 live_checksum={:?}; refusing settlement",
                plan.er_slot, checkpoint.effect_commitment, plan.checksum,
            );
            return None;
        }

        match checkpoint.status {
            CheckpointStatus::Pending => {
                if l1_bank.slot() < checkpoint.challenge_deadline_l1_slot {
                    info!(
                        "Portal checkpoint waiting: er_slot={} current_l1_slot={} deadline={}",
                        plan.er_slot,
                        l1_bank.slot(),
                        checkpoint.challenge_deadline_l1_slot,
                    );
                    return None;
                }
                info!(
                    "Portal checkpoint commit then settle: er_slot={} checksum={:?}",
                    plan.er_slot, plan.checksum,
                );
                let mut transactions = vec![plan.checkpoint_commit_transaction(
                    self.config.portal_program_id,
                    session_pda,
                    self.config.manager_account.as_ref(),
                    recent_blockhash,
                )];
                transactions.extend(self.settlement_transactions_for_plan(
                    plan,
                    session_pda,
                    recent_blockhash,
                    true,
                ));
                Some(transactions)
            }
            CheckpointStatus::Committed => {
                info!(
                    "Portal checkpoint committed; settling er_slot={} checksum={:?}",
                    plan.er_slot, plan.checksum,
                );
                Some(self.settlement_transactions_for_plan(
                    plan,
                    session_pda,
                    recent_blockhash,
                    true,
                ))
            }
            CheckpointStatus::Challenged => {
                warn!(
                    "Portal checkpoint challenged; explicit resolution required: er_slot={} \
                     checkpoint={} current_l1_slot={} hard_deadline={}",
                    plan.er_slot,
                    checkpoint_pda,
                    l1_bank.slot(),
                    checkpoint.challenge_deadline_l1_slot,
                );
                None
            }
            CheckpointStatus::Settled => {
                if Self::token_withdrawals_complete(l1_bank, plan) {
                    debug!(
                        "Portal checkpoint fully settled: er_slot={} checkpoint={}",
                        plan.er_slot, checkpoint_pda,
                    );
                    self.remove_checkpoint_plan(session_pda, &checkpoint);
                    None
                } else {
                    warn!(
                        "Portal checkpoint has pending token releases: er_slot={} checkpoint={}",
                        plan.er_slot, checkpoint_pda,
                    );
                    Some(settlement::token_withdrawal_transactions(
                        &plan.token_withdrawals,
                        self.config.portal_program_id,
                        session_pda,
                        plan.er_slot,
                        plan.checksum,
                        self.config.manager_account.as_ref(),
                        recent_blockhash,
                    ))
                }
            }
            CheckpointStatus::Cancelled => {
                warn!(
                    "Portal checkpoint cancelled: er_slot={} checkpoint={}",
                    plan.er_slot, checkpoint_pda,
                );
                self.remove_checkpoint_plan(session_pda, &checkpoint);
                None
            }
            CheckpointStatus::Invalid => {
                warn!(
                    "Portal checkpoint invalid: er_slot={} checkpoint={}",
                    plan.er_slot, checkpoint_pda,
                );
                self.remove_checkpoint_plan(session_pda, &checkpoint);
                None
            }
        }
    }

    /// Sonic: Shutdown the always-on runtime (called at validator exit)
    pub fn shutdown_runtime(&mut self) {
        if let Some(mut runtime) = self.runtime.take() {
            info!("Shutting down ephemeral rollup runtime");
            runtime.shutdown();
        }
    }

    fn parse_event(
        &self,
        bank: &Bank,
        pubkey: Pubkey,
        account: AccountSharedData,
    ) -> Option<L1Event> {
        let data = account.data();
        // Account was zeroed — determine type from previous state
        if data.iter().all(|b| *b == 0) {
            return self.parse_zeroed_account(bank, &pubkey);
        }

        match try_parse_raw_portal_account(data) {
            // Check if this is a new session (didn't exist in parent)
            Some(PortalAccount::Session(session)) => {
                let (expected_session, session_bump) =
                    northstar_portal::find_session_pda(&self.config.portal_program_id);
                if !session.is_valid() || pubkey != expected_session || session.bump != session_bump
                {
                    return None;
                }
                self.is_new_in_slot(bank, &pubkey)
                    .then_some(L1Event::SessionOpened {
                        session_pda: pubkey,
                        grid_id: session.grid_id,
                        ttl_slots: session.ttl_slots,
                        fee_cap: session.fee_cap,
                    })
            }
            Some(PortalAccount::DelegationRecord(_)) if !self.is_new_in_slot(bank, &pubkey) => None,
            Some(PortalAccount::DelegationRecord(record)) => self
                .find_delegated_account(bank, &pubkey)
                .map(|delegated| L1Event::AccountDelegated {
                    delegation_record_pda: pubkey,
                    delegated_account: delegated,
                    owner_program: record.owner_program,
                    grid_id: record.grid_id,
                }),
            Some(PortalAccount::FeeVault(_vault)) => {
                // FeeVault balance tracking removed; events come from DepositReceipts
                None
            }
            Some(PortalAccount::DepositReceipt(receipt)) => {
                let expected_session =
                    northstar_portal::find_session_pda(&self.config.portal_program_id).0;
                let (expected_receipt, receipt_bump) = northstar_portal::find_deposit_receipt_pda(
                    &self.config.portal_program_id,
                    &receipt.session,
                    &receipt.recipient,
                );
                if !receipt.is_valid()
                    || receipt.session != expected_session
                    || pubkey != expected_receipt
                    || receipt.bump != receipt_bump
                {
                    return None;
                }
                let session_account = bank.get_account(&receipt.session)?;
                if session_account.owner() != &self.config.portal_program_id {
                    return None;
                }
                let PortalAccount::Session(session_state) =
                    portal_state::try_parse_raw_portal_account(session_account.data())?
                else {
                    return None;
                };
                if !session_state.is_valid() {
                    return None;
                }
                let prev_escrow = bank
                    .parent()
                    .and_then(|parent| {
                        let account = parent.get_account(&pubkey)?;
                        portal_state::try_parse_raw_portal_account(account.data())?;
                        Some(deposit_receipt_escrow_lamports(
                            &parent,
                            account.lamports(),
                            account.data().len(),
                        ))
                    })
                    .unwrap_or(0);
                let escrow =
                    deposit_receipt_escrow_lamports(bank, account.lamports(), account.data().len());

                (escrow > prev_escrow).then(|| L1Event::FeeDeposited {
                    session_pda: receipt.session,
                    amount: escrow,
                    delta: escrow - prev_escrow,
                    depositor: receipt.recipient,
                })
            }
            Some(PortalAccount::Checkpoint(_))
            | Some(PortalAccount::CheckpointCursor(_))
            | Some(PortalAccount::Challenge(_))
            | Some(PortalAccount::DataAvailabilityProof(_))
            | Some(PortalAccount::StepProofAccount(_))
            | Some(PortalAccount::SessionBridge(_))
            | Some(PortalAccount::TokenWithdrawalAuthorization(_)) => None,
            None => {
                // Unrecognized — log and skip
                debug!("Unrecognized portal account at {pubkey}");
                None
            }
        }
    }

    fn parse_token_bridge_event(
        &self,
        bank: &Bank,
        pubkey: Pubkey,
        account: &AccountSharedData,
    ) -> Option<L1Event> {
        let receipt =
            borsh::from_slice::<northstar_token_bridge::state::TokenDepositReceipt>(account.data())
                .ok()?;
        if !receipt.is_valid() {
            return None;
        }

        let session_bridge_key = Pubkey::new_from_array(receipt.session_bridge);
        let session_bridge_account = bank.get_account(&session_bridge_key)?;
        if session_bridge_account.owner() != &self.config.portal_program_id {
            return None;
        }
        let PortalAccount::SessionBridge(session_bridge) =
            portal_state::try_parse_raw_portal_account(session_bridge_account.data())?
        else {
            return None;
        };
        let expected_session = northstar_portal::find_session_pda(&self.config.portal_program_id).0;
        let (expected_session_bridge, session_bridge_bump) =
            northstar_portal::find_session_bridge_pda(
                &self.config.portal_program_id,
                &session_bridge.session,
                &session_bridge.mint,
            );
        if !session_bridge.is_valid()
            || session_bridge.session != expected_session
            || session_bridge_key != expected_session_bridge
            || session_bridge.bump != session_bridge_bump
        {
            return None;
        }
        let bridge_program = session_bridge.bridge_program;
        if account.owner() != &bridge_program {
            return None;
        }
        let er_token_account = Pubkey::new_from_array(receipt.er_token_account);
        let (expected_receipt, receipt_bump) =
            northstar_token_bridge::find_token_deposit_receipt_pda(
                &bridge_program,
                &session_bridge_key,
                &er_token_account,
            );
        if pubkey != expected_receipt || receipt.bump != receipt_bump {
            return None;
        }

        let previous_balance = bank
            .parent()
            .and_then(|parent| parent.get_account(&pubkey))
            .filter(|previous| previous.owner() == &bridge_program)
            .and_then(|previous| {
                borsh::from_slice::<northstar_token_bridge::state::TokenDepositReceipt>(
                    previous.data(),
                )
                .ok()
            })
            .filter(|previous| {
                previous.is_valid()
                    && previous.session_bridge == receipt.session_bridge
                    && previous.er_token_account == receipt.er_token_account
            })
            .map(|previous| previous.balance)
            .unwrap_or(0);
        (receipt.balance > previous_balance).then_some(L1Event::TokenDeposited {
            session_pda: session_bridge.session,
            session_bridge: session_bridge_key,
            bridge_program,
            er_token_account,
            amount: receipt.balance,
            delta: receipt.balance - previous_balance,
        })
    }

    pub fn get_l1_events(&self, bank: &Bank) -> Vec<L1Event> {
        // IMPORTANT: Do not use `get_all_accounts_modified_since_parent` here. It walks the
        // global AccountsIndex for every frozen bank and can permanently starve L1 event
        // ingestion. The slot-local storage scan still includes closed Portal accounts.
        bank.get_accounts_modified_in_slot()
            .into_iter()
            .filter_map(|(pubkey, account)| {
                if account.owner() == &self.config.portal_program_id {
                    self.parse_event(bank, pubkey, account)
                } else {
                    bank.parent()
                        .and_then(|parent| parent.get_account(&pubkey))
                        .filter(|parent_account| {
                            parent_account.owner() == &self.config.portal_program_id
                        })
                        .and_then(|_| self.parse_zeroed_account(bank, &pubkey))
                        .or_else(|| self.parse_token_bridge_event(bank, pubkey, &account))
                }
            })
            .collect()
    }

    /// Check if an account existed in the parent bank
    fn is_new_in_slot(&self, bank: &Bank, pubkey: &Pubkey) -> bool {
        bank.parent()
            .map(|parent| parent.get_account(pubkey).is_none())
            .unwrap_or(true)
    }

    fn find_delegated_account(
        &self,
        bank: &Bank,
        delegation_record_pda: &Pubkey,
    ) -> Option<Pubkey> {
        let undelegated_account = bank
            .get_accounts_modified_in_slot()
            .into_iter()
            .filter(|(pubkey, _)| pubkey != delegation_record_pda)
            .filter(|(_, account)| account.owner() == &self.config.portal_program_id)
            .find_map(|(pubkey, _)| {
                // Verify PDA derivation
                let (expected_pda, _) = Pubkey::find_program_address(
                    &[b"delegation", pubkey.as_ref()],
                    &self.config.portal_program_id,
                );
                (&expected_pda == delegation_record_pda).then_some(pubkey)
            });

        if undelegated_account.is_none() {
            warn!(
                "Could not find undelegated account for delegation record {}",
                delegation_record_pda
            );
        }
        undelegated_account
    }

    fn delegation_record(
        &self,
        bank: &Bank,
        delegated_account: &Pubkey,
    ) -> Option<northstar_portal::DelegationRecord> {
        let (record_pubkey, _) =
            find_portal_delegation_record_pda(&self.config.portal_program_id, delegated_account);
        let record_account = bank.get_account(&record_pubkey)?;
        let PortalAccount::DelegationRecord(record) =
            try_parse_raw_portal_account(record_account.data())?
        else {
            return None;
        };
        Some(record)
    }

    fn find_undelegated_account(
        &self,
        bank: &Bank,
        delegation_record_pda: &Pubkey,
    ) -> Option<Pubkey> {
        let parent = bank.parent()?;
        let undelegated_account = bank
            .get_accounts_modified_in_slot()
            .into_iter()
            .filter(|(pubkey, _)| pubkey != delegation_record_pda)
            .filter(|(pubkey, account)| {
                // Check that account is not owned by portal now,
                // but was owned a block ago
                account.owner() != &self.config.portal_program_id
                    && parent
                        .get_account(pubkey)
                        .map(|a| a.owner() == &self.config.portal_program_id)
                        .unwrap_or_default()
            })
            .find_map(|(pubkey, _)| {
                // Verify PDA derivation
                let (expected_pda, _) = Pubkey::find_program_address(
                    &[b"delegation", pubkey.as_ref()],
                    &self.config.portal_program_id,
                );
                (&expected_pda == delegation_record_pda).then_some(pubkey)
            });

        if undelegated_account.is_none() {
            warn!(
                "Could not find undelegated account for delegation record {}",
                delegation_record_pda
            );
        }
        undelegated_account
    }

    /// When an account's data is zeroed, determine what type it was from the parent bank
    fn parse_zeroed_account(&self, bank: &Bank, pubkey: &Pubkey) -> Option<L1Event> {
        let prev_account = bank.parent()?.get_account(pubkey)?;

        match try_parse_raw_portal_account(prev_account.data())? {
            PortalAccount::Session(session) => {
                let (expected_session, session_bump) =
                    northstar_portal::find_session_pda(&self.config.portal_program_id);
                (session.is_valid() && *pubkey == expected_session && session.bump == session_bump)
                    .then_some(L1Event::SessionClosed {
                        session_pda: *pubkey,
                        grid_id: session.grid_id,
                    })
            }
            // Find the delegated account that was undelegated
            // by scanning for accounts whose owner changed FROM portal
            PortalAccount::DelegationRecord(_record) => Some(L1Event::AccountUndelegated {
                delegation_record_pda: *pubkey,
                delegated_account: self.find_undelegated_account(bank, pubkey)?,
            }),
            _ => None,
        }
    }

    /// Sonic: Initialize the always-on ephemeral RPC runtime.
    /// Called once at validator startup. RPC starts listening immediately
    /// but rejects transactions until `activate_session()` is called.
    pub fn init_runtime(
        &mut self,
        root_bank: Arc<Bank>,
        cluster_info: Arc<ClusterInfo>,
        rpc_addr: SocketAddr,
        ws_addr: SocketAddr,
        tpu_addr: SocketAddr,
    ) -> Result<()> {
        if self.runtime.is_some() {
            info!("Ephemeral runtime already initialized, skipping");
            return Ok(());
        }

        trace!(
            "init_runtime: root_bank slot={}, epoch={}, slots_per_epoch={}",
            root_bank.slot(),
            root_bank.epoch(),
            root_bank.get_slots_in_epoch(root_bank.epoch()),
        );

        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::default(),
            grid_id: 0,
            ttl_slots: 0,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
            fee_cap: 0,
            delegated_accounts: vec![],
        };

        let runtime = EphemeralRuntime::new_with_slot_duration(
            root_bank,
            cluster_info,
            settings,
            rpc_addr,
            ws_addr,
            tpu_addr,
            self.config.portal_program_id,
            self.config.manager_account.clone(),
            self.slot_duration,
            self.er_history_max_retained_slots,
        )
        .map_err(|e| {
            error!("Failed to create ephemeral runtime: {}", e);
            NorthStarError::RuntimeCreationFailed(e)
        })?;

        if let Some(dir) = self.unsettled_state_dir() {
            runtime.configure_unsettled_state(dir);
        }

        info!(
            "Always-on ephemeral RPC initialized at {rpc_addr}, WS at {ws_addr}, TPU at \
             {tpu_addr} (inactive until session opens)"
        );
        self.runtime = Some(runtime);
        Ok(())
    }

    fn prepare_session(
        &mut self,
        root_bank: Arc<Bank>,
        session_pda: Pubkey,
        session: &northstar_portal::Session,
    ) -> Option<RecoveredUnsettledState> {
        let persistence_enabled = self.unsettled_state_dir().is_some();
        let Some(runtime) = &mut self.runtime else {
            warn!("Cannot activate session: runtime not initialized");
            return None;
        };
        trace!(
            "activate_session: resetting to L1 root slot={}, epoch={}",
            root_bank.slot(),
            root_bank.epoch(),
        );
        runtime.set_session_settings(session.grid_id, session.ttl_slots, session.fee_cap);
        runtime.reset_to_new_parent(root_bank);
        runtime.set_session_pda(session_pda);
        if !persistence_enabled {
            return Some(RecoveredUnsettledState {
                accounts: vec![],
                touched_accounts: vec![],
                payout_events: vec![],
                token_payout_events: vec![],
                processed_signatures: vec![],
            });
        }

        let identity =
            UnsettledSessionIdentity::new(self.config.portal_program_id, session_pda, session);
        match runtime.begin_unsettled_session(identity) {
            Ok(outcome) => {
                match outcome.disposition {
                    RecoveryDisposition::Recovered => info!(
                        "Recovered persisted unsettled ER state: accounts={}, SOL payouts={}, \
                         token payouts={}, signatures={}",
                        outcome.state.accounts.len(),
                        outcome.state.payout_events.len(),
                        outcome.state.token_payout_events.len(),
                        outcome.state.processed_signatures.len(),
                    ),
                    RecoveryDisposition::DroppedIdentityMismatch => warn!(
                        "Dropped persisted unsettled ER state because Portal/session \
                         configuration changed"
                    ),
                    RecoveryDisposition::DroppedCorrupt => {
                        warn!("Dropped corrupt persisted unsettled ER state")
                    }
                    RecoveryDisposition::New => {}
                }
                Some(outcome.state)
            }
            Err(err) => {
                warn!("Cannot activate ER: failed to initialize unsettled state journal: {err}");
                None
            }
        }
    }

    /// Sonic: Activate the ephemeral session — resets bank to current L1 root
    /// and starts accepting transactions.
    pub fn activate_session(
        &mut self,
        root_bank: Arc<Bank>,
        session_pda: Pubkey,
        grid_id: u64,
        ttl_slots: u64,
        fee_cap: u64,
    ) {
        let Some(account) = root_bank.get_account(&session_pda) else {
            warn!("Cannot activate session: Session account {session_pda} is missing");
            return;
        };
        let Some(PortalAccount::Session(session)) = try_parse_raw_portal_account(account.data())
        else {
            warn!("Cannot activate session: Session account {session_pda} is invalid");
            return;
        };
        if session.grid_id != grid_id
            || session.ttl_slots != ttl_slots
            || session.fee_cap != fee_cap
        {
            warn!("Cannot activate session: event settings do not match Session account");
            return;
        }
        let Some(recovered) = self.prepare_session(root_bank, session_pda, &session) else {
            return;
        };
        if let Some(runtime) = &mut self.runtime {
            runtime.restore_unsettled_state(recovered);
            runtime.enable_unsettled_writes();
            runtime.activate();
            info!(
                "Ephemeral session activated, PDA={session_pda}, grid_id={}",
                session.grid_id
            );
        }
    }

    /// Sonic: Deactivate the ephemeral session — transactions will be rejected.
    pub fn deactivate_session(&mut self) {
        if let Some(runtime) = &mut self.runtime {
            runtime.deactivate();
            runtime.clear_unsettled_state();
            info!("Ephemeral session deactivated and persisted state cleared");
        } else {
            warn!("Cannot deactivate session: runtime not initialized");
        }
    }

    fn active_l1_session(&self, bank: &Bank) -> Option<(Pubkey, northstar_portal::Session)> {
        let (session_pda, _) = northstar_portal::find_session_pda(&self.config.portal_program_id);
        let account = bank.get_account(&session_pda)?;
        if account.owner() != &self.config.portal_program_id {
            return None;
        }
        let PortalAccount::Session(session) = try_parse_raw_portal_account(account.data())? else {
            return None;
        };
        if !session.is_valid()
            || session.is_expired(bank.slot())
            || session.validator != self.config.manager_account.pubkey()
        {
            return None;
        }
        Some((session_pda, session))
    }

    fn l1_delegations_for_grid(
        &self,
        bank: &Bank,
        grid_id: u64,
    ) -> Vec<(Pubkey, AccountSharedData, Pubkey)> {
        // IMPORTANT: Never fall back to `get_program_accounts` here. Without a program-id
        // secondary index it scans every account during startup and can stall session resume
        // for minutes. Portal validators install a targeted index before loading AccountsDb.
        let program_accounts = match bank.get_filtered_indexed_accounts(
            &IndexKey::ProgramId(self.config.portal_program_id),
            |account| account.owner() == &self.config.portal_program_id,
            None,
        ) {
            Ok(accounts) => accounts,
            Err(err) => {
                warn!("Failed to load indexed Portal accounts for startup resume: {err:?}");
                return vec![];
            }
        };

        let records = program_accounts
            .iter()
            .filter_map(|(record_pubkey, account)| {
                let PortalAccount::DelegationRecord(record) =
                    try_parse_raw_portal_account(account.data())?
                else {
                    return None;
                };
                (record.is_valid() && record.grid_id == grid_id).then_some((*record_pubkey, record))
            })
            .collect::<Vec<_>>();

        let program_id = self.config.portal_program_id;
        let delegated_by_record = program_accounts
            .iter()
            .map(|(candidate, account)| {
                let (record_pubkey, _) = find_portal_delegation_record_pda(&program_id, candidate);
                (record_pubkey, (*candidate, account.clone()))
            })
            .collect::<HashMap<_, _>>();

        records
            .into_iter()
            .filter_map(|(record_pubkey, record)| {
                delegated_by_record
                    .get(&record_pubkey)
                    .map(|(delegated, account)| (*delegated, account.clone(), record.owner_program))
            })
            .collect()
    }

    /// Resume an active ER session from current L1 state at validator startup.
    pub fn resume_active_session_from_l1(&mut self, root_bank: Arc<Bank>) -> bool {
        if self.runtime.is_none() {
            warn!("Cannot resume session from L1: runtime not initialized");
            return false;
        }
        if self.has_active_runtime() {
            return false;
        }

        let Some((session_pda, session)) = self.active_l1_session(&root_bank) else {
            return false;
        };
        let delegations = self.l1_delegations_for_grid(&root_bank, session.grid_id);
        if session.settlement_status == SettlementStatus::InProgress {
            warn!(
                "Resuming ER while Portal settlement is InProgress: PDA={}, er_slot={}, \
                 checksum={:?}. New settlements stay blocked until the existing settlement \
                 finishes or aborts on L1.",
                session_pda, session.settlement_er_slot, session.settlement_checksum,
            );
        }

        let Some(recovered) = self.prepare_session(root_bank.clone(), session_pda, &session) else {
            return false;
        };
        for (delegated_account, account, owner_program) in &delegations {
            if let Some(runtime) = &self.runtime {
                runtime.handle_delegation_inner(
                    delegated_account,
                    account.clone(),
                    Some(*owner_program),
                    Some(&root_bank),
                );
            }
        }
        if let Some(runtime) = &mut self.runtime {
            runtime.restore_unsettled_state(recovered);
            runtime.enable_unsettled_writes();
            runtime.activate();
        }
        self.mark_synced_through(root_bank.slot());

        info!(
            "Resumed ephemeral session from L1 at slot {}, PDA={}, grid_id={}, \
             delegated_accounts={}",
            root_bank.slot(),
            session_pda,
            session.grid_id,
            delegations.len(),
        );
        true
    }

    /// Create and store an EphemeralRuntime from the root bank
    ///
    /// This creates a fully functional ephemeral rollup with its own RPC server.
    /// The runtime is stored in the Manager and can be accessed via runtime_addr().
    #[cfg(test)]
    pub fn create_ephemeral_runtime(
        &mut self,
        root_bank: Arc<Bank>,
        cluster_info: Arc<ClusterInfo>,
        settings: EphemeralRollupSettings,
        rpc_addr: SocketAddr,
    ) -> Result<()> {
        if self.runtime.is_some() {
            info!("Ephemeral runtime already exists, skipping creation");
            return Ok(());
        }

        let mut runtime = EphemeralRuntime::new_with_slot_duration(
            root_bank,
            cluster_info,
            settings,
            rpc_addr,
            // Tests: no WS or TPU — use unbound addrs that won't be used
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
            self.config.portal_program_id,
            self.config.manager_account.clone(),
            self.slot_duration,
            self.er_history_max_retained_slots,
        )
        .map_err(|e| {
            error!("Failed to create ephemeral runtime: {}", e);
            NorthStarError::RuntimeCreationFailed(e)
        })?;

        info!("Ephemeral rollup started on {}", rpc_addr);
        runtime.activate();
        self.runtime = Some(runtime);
        Ok(())
    }

    /// Credit a deposit to a depositor's account on the ephemeral bank.
    /// Called by NorthStarService when a FeeDeposited event is detected on L1.
    /// Only processes when a session is active.
    pub fn credit_deposit(&self, depositor: &Pubkey, lamports: u64) {
        if let Some(runtime) = &self.runtime {
            if !runtime.is_active() {
                warn!("Ignoring deposit for {depositor}: no active session");
                return;
            }
            runtime.credit_deposit(depositor, lamports);
        }
    }

    pub fn credit_token_deposit(
        &self,
        bridge_program: &Pubkey,
        session_bridge: &Pubkey,
        er_token_account: &Pubkey,
        amount: u64,
    ) {
        if let Some(runtime) = &self.runtime {
            if !runtime.is_active() {
                warn!("Ignoring token deposit for {er_token_account}: no active session");
                return;
            }
            runtime.credit_token_deposit(bridge_program, session_bridge, er_token_account, amount);
        }
    }

    /// Re-anchor the active ER onto the latest L1 block while preserving the
    /// in-memory ER account overlay.
    pub fn reanchor_to_l1_parent(&mut self, bank: Arc<Bank>) {
        if let Some(runtime) = &mut self.runtime {
            if !runtime.is_active() {
                return;
            }
            runtime.reanchor_to_l1_parent(bank);
        }
    }

    /// Refresh delegated owner programs from L1 when their deployment accounts
    /// changed. This catches L1 `solana program deploy` updates that do not
    /// emit Portal events but must still invalidate the isolated ER ProgramCache.
    pub fn refresh_delegated_owner_programs(&self, bank: &Bank) {
        if let Some(runtime) = &self.runtime {
            if !runtime.is_active() {
                return;
            }
            runtime.refresh_delegated_owner_programs_from_l1(bank);
        }
    }

    /// Handle a new account delegation from L1.
    /// Called by NorthStarService when an AccountDelegated event is detected on L1.
    /// Copies the account data from L1 into the ephemeral bank and adds it to
    /// the delegated set so transactions are allowed to write to it.
    /// Only processes when a session is active.
    pub fn handle_delegation(&self, bank: &Bank, delegated_account: &Pubkey) {
        let owner_program = self
            .delegation_record(bank, delegated_account)
            .map(|record| record.owner_program);
        self.handle_delegation_with_owner_program(bank, delegated_account, owner_program);
    }

    pub fn handle_delegation_with_owner_program(
        &self,
        bank: &Bank,
        delegated_account: &Pubkey,
        owner_program: Option<Pubkey>,
    ) {
        if let Some(runtime) = &self.runtime {
            if !runtime.is_active() {
                warn!("Ignoring delegation for {delegated_account}: no active session");
                return;
            }
            if let Some(account_data) = bank.get_account(delegated_account) {
                runtime.handle_delegation_inner(
                    delegated_account,
                    account_data,
                    owner_program,
                    Some(bank),
                );
            } else {
                warn!(
                    "Cannot handle delegation: account {} not found on L1",
                    delegated_account
                );
            }
        }
    }
}

#[cfg(test)]
mod portal_e2e_tests {
    use {
        super::*,
        agave_logger::setup,
        northstar_portal::{
            Checkpoint, CheckpointCursor, CheckpointStatus, CommitCheckpoint, DelegationRecord,
            OpenChallenge, OpenSession, PortalInstruction, ProposeCheckpoint, Session,
        },
        solana_account::{AccountSharedData, WritableAccount},
        solana_accounts_db::{
            accounts_db::{AccountsDbConfig, ACCOUNTS_DB_CONFIG_FOR_TESTING},
            accounts_index::{
                AccountIndex, AccountSecondaryIndexes, AccountSecondaryIndexesIncludeExclude,
            },
        },
        solana_gossip::contact_info::ContactInfo,
        solana_instruction::{AccountMeta, Instruction},
        solana_keypair::{Keypair, Signer},
        solana_lattice_hash::lt_hash::LtHash,
        solana_leader_schedule::SlotLeader,
        solana_net_utils::SocketAddrSpace,
        solana_rent::Rent,
        solana_rpc_client::rpc_client::RpcClient,
        solana_runtime::{
            bank::BankTestConfig,
            bank_forks::BankForks,
            genesis_utils::{create_genesis_config, GenesisConfigInfo},
        },
        solana_sdk_ids::system_program,
        solana_system_interface::instruction::transfer,
        solana_transaction::Transaction,
        std::{
            collections::HashSet,
            sync::RwLock,
            time::{Duration, Instant},
        },
    };

    /// Set up a test bank with portal program in genesis.
    /// Returns (bank, bank_forks, program_id, mint_keypair).
    fn setup_bank_with_portal() -> (Arc<Bank>, Arc<RwLock<BankForks>>, Pubkey, Keypair) {
        let GenesisConfigInfo {
            mut genesis_config,
            mint_keypair,
            ..
        } = create_genesis_config(1_000_000_000_000);
        genesis_config.rent = Rent::default();

        let program_id = Pubkey::new_unique();
        let program_data = solana_runtime::loader_utils::load_program_from_file("northstar_portal");
        genesis_config.accounts.insert(
            program_id,
            solana_account::Account {
                lamports: genesis_config
                    .rent
                    .minimum_balance(program_data.len())
                    .max(1),
                data: program_data,
                owner: solana_sdk_ids::bpf_loader::id(),
                executable: true,
                rent_epoch: 0,
            },
        );

        let account_indexes = AccountSecondaryIndexes {
            keys: Some(AccountSecondaryIndexesIncludeExclude {
                exclude: false,
                keys: HashSet::from([program_id]),
            }),
            indexes: HashSet::from([AccountIndex::ProgramId]),
        };
        let bank = Arc::new(Bank::new_with_paths_for_tests(
            &genesis_config,
            Some(BankTestConfig {
                accounts_db_config: AccountsDbConfig {
                    account_indexes: Some(account_indexes),
                    ..ACCOUNTS_DB_CONFIG_FOR_TESTING
                },
            }),
            vec![],
            None,
        ));
        bank.fill_bank_with_ticks_for_tests();
        let bank = Bank::new_from_parent(bank.clone(), *bank.leader(), bank.slot() + 1);
        let bank_forks = BankForks::new_rw_arc(bank);
        let bank = Arc::clone(&bank_forks.read().unwrap().root_bank());
        (bank, bank_forks, program_id, mint_keypair)
    }

    fn find_session_pda(program_id: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[b"session"], program_id)
    }

    fn find_fee_vault_pda(program_id: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[b"fee_vault"], program_id)
    }

    fn find_delegation_record_pda(program_id: &Pubkey, delegated_account: &Pubkey) -> (Pubkey, u8) {
        let (pda, bump) = find_portal_delegation_record_pda(program_id, delegated_account);
        (pda, bump)
    }

    fn find_checkpoint_pda(program_id: &Pubkey, session: &Pubkey, er_slot: u64) -> (Pubkey, u8) {
        let (pda, bump) = northstar_portal::find_checkpoint_pda(program_id, session, er_slot);
        (pda, bump)
    }

    fn find_checkpoint_cursor_pda(program_id: &Pubkey, session: &Pubkey) -> (Pubkey, u8) {
        let (pda, bump) = northstar_portal::find_checkpoint_cursor_pda(program_id, session);
        (pda, bump)
    }

    fn store_delegation_record(
        bank: &Bank,
        program_id: &Pubkey,
        delegated_account: &Pubkey,
        owner_program: &Pubkey,
        grid_id: u64,
    ) {
        let (record_pubkey, bump) = find_delegation_record_pda(program_id, delegated_account);
        let record = DelegationRecord {
            discriminator: DelegationRecord::DISCRIMINATOR,
            owner_program: *owner_program,
            grid_id,
            bump,
        };
        let data = borsh::to_vec(&record).unwrap();
        let mut account = AccountSharedData::new(1_000_000, data.len(), program_id);
        account.data_as_mut_slice().copy_from_slice(&data);
        bank.store_account(&record_pubkey, &account);
    }

    fn find_deposit_receipt_pda(
        program_id: &Pubkey,
        session: &Pubkey,
        recipient: &Pubkey,
    ) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[b"deposit_receipt", session.as_ref(), recipient.as_ref()],
            program_id,
        )
    }

    fn build_open_session_ix(
        program_id: Pubkey,
        owner: Pubkey,
        session_pda: Pubkey,
        fee_vault_pda: Pubkey,
        grid_id: u64,
        ttl_slots: u64,
        fee_cap: u64,
    ) -> Instruction {
        let ix = PortalInstruction::OpenSession(OpenSession {
            grid_id,
            ttl_slots,
            fee_cap,
            validator: owner,
            settlement_interval_slots: 10,
        });
        let data = borsh::to_vec(&ix).unwrap();
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(owner, true),
                AccountMeta::new(session_pda, false),
                AccountMeta::new(fee_vault_pda, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data,
        }
    }

    fn store_session(
        bank: &Bank,
        program_id: &Pubkey,
        session: &Pubkey,
        bump: u8,
        validator: &Pubkey,
        grid_id: u64,
        settlement_interval_slots: u64,
    ) {
        let session_state = Session {
            discriminator: Session::DISCRIMINATOR,
            grid_id,
            ttl_slots: 1_000,
            fee_cap: 123_456,
            created_at: bank.slot(),
            nonce: 1,
            authority: Pubkey::new_unique(),
            validator: *validator,
            settlement_interval_slots,
            last_settled_l1_slot: bank.slot(),
            last_settled_er_slot: 0,
            settlement_status: SettlementStatus::Idle,
            settlement_er_slot: 0,
            settlement_checksum: [0; 32],
            settlement_accumulator: [0; 32],
            settlement_started_l1_slot: 0,
            bump,
        };
        let data = borsh::to_vec(&session_state).unwrap();
        let mut account = AccountSharedData::new(1_000_000, data.len(), program_id);
        account.data_as_mut_slice().copy_from_slice(&data);
        bank.store_account(session, &account);
    }

    fn store_committed_checkpoint(
        bank: &Bank,
        program_id: &Pubkey,
        session: &Pubkey,
        er_slot: u64,
        effect_commitment: [u8; 32],
        proposer: &Pubkey,
    ) -> Pubkey {
        let (checkpoint_pda, bump) = find_checkpoint_pda(program_id, session, er_slot);
        let checkpoint = Checkpoint {
            discriminator: Checkpoint::DISCRIMINATOR,
            session: *session,
            er_slot,
            step_count: 1,
            previous_state_root: [0; 32],
            new_state_root: [0; 32],
            trace_root: effect_commitment,
            tx_effect_root: effect_commitment,
            readonly_l1_root: [0; 32],
            da_commitment: effect_commitment,
            effect_commitment,
            proposer: *proposer,
            proposed_at_l1_slot: bank.slot(),
            challenge_deadline_l1_slot: bank.slot(),
            status: CheckpointStatus::Committed,
            bond_lamports: 0,
            bond_status: northstar_portal::CheckpointBondStatus::Released,
            challenger: Pubkey::default(),
            challenged_at_l1_slot: 0,
            challenge_resolved: false,
            bump,
        };
        let data = borsh::to_vec(&checkpoint).unwrap();
        let mut account = AccountSharedData::new(1_000_000, data.len(), program_id);
        account.data_as_mut_slice().copy_from_slice(&data);
        bank.store_account(&checkpoint_pda, &account);

        let (cursor_pda, cursor_bump) = find_checkpoint_cursor_pda(program_id, session);
        let cursor = CheckpointCursor {
            discriminator: CheckpointCursor::DISCRIMINATOR,
            session: *session,
            latest_finalized_checkpoint: checkpoint_pda,
            latest_finalized_er_slot: er_slot,
            latest_finalized_state_root: checkpoint.new_state_root,
            active_checkpoint: checkpoint_pda,
            active_er_slot: er_slot,
            bump: cursor_bump,
        };
        let data = borsh::to_vec(&cursor).unwrap();
        let mut account = AccountSharedData::new(1_000_000, data.len(), program_id);
        account.data_as_mut_slice().copy_from_slice(&data);
        bank.store_account(&cursor_pda, &account);

        checkpoint_pda
    }

    fn build_propose_checkpoint_ix(
        program_id: Pubkey,
        proposer: Pubkey,
        session_pda: Pubkey,
        er_slot: u64,
        effect_commitment: [u8; 32],
        challenge_window_slots: u64,
    ) -> Instruction {
        let (checkpoint_pda, _) = find_checkpoint_pda(&program_id, &session_pda, er_slot);
        let (cursor_pda, _) = find_checkpoint_cursor_pda(&program_id, &session_pda);
        let ix = PortalInstruction::ProposeCheckpoint(ProposeCheckpoint {
            er_slot,
            step_count: 1,
            previous_state_root: [0; 32],
            new_state_root: [1; 32],
            trace_root: effect_commitment,
            tx_effect_root: effect_commitment,
            readonly_l1_root: [0; 32],
            da_commitment: effect_commitment,
            effect_commitment,
            challenge_window_slots,
        });
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(proposer, true),
                AccountMeta::new_readonly(session_pda, false),
                AccountMeta::new(checkpoint_pda, false),
                AccountMeta::new(cursor_pda, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data: borsh::to_vec(&ix).unwrap(),
        }
    }

    fn build_commit_checkpoint_ix(
        program_id: Pubkey,
        committer: Pubkey,
        proposer: Pubkey,
        session_pda: Pubkey,
        er_slot: u64,
    ) -> Instruction {
        let (checkpoint_pda, _) = find_checkpoint_pda(&program_id, &session_pda, er_slot);
        let (cursor_pda, _) = find_checkpoint_cursor_pda(&program_id, &session_pda);
        let ix = PortalInstruction::CommitCheckpoint(CommitCheckpoint { er_slot });
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new_readonly(committer, true),
                AccountMeta::new_readonly(session_pda, false),
                AccountMeta::new(checkpoint_pda, false),
                AccountMeta::new(cursor_pda, false),
                AccountMeta::new(proposer, false),
            ],
            data: borsh::to_vec(&ix).unwrap(),
        }
    }

    fn build_challenge_checkpoint_ix(
        program_id: Pubkey,
        challenger: Pubkey,
        session_pda: Pubkey,
        er_slot: u64,
    ) -> Instruction {
        let (checkpoint_pda, _) = find_checkpoint_pda(&program_id, &session_pda, er_slot);
        let (challenge, _) = northstar_portal::find_challenge_pda(&program_id, &checkpoint_pda);
        let (da_proof, _) = northstar_portal::find_da_proof_pda(&program_id, &challenge);
        let ix = PortalInstruction::OpenChallenge(OpenChallenge { er_slot });
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(challenger, true),
                AccountMeta::new_readonly(session_pda, false),
                AccountMeta::new(checkpoint_pda, false),
                AccountMeta::new(challenge, false),
                AccountMeta::new(da_proof, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data: borsh::to_vec(&ix).unwrap(),
        }
    }

    fn build_deposit_fee_ix(
        program_id: Pubkey,
        depositor: Pubkey,
        session_pda: Pubkey,
        recipient: Pubkey,
        lamports: u64,
    ) -> Instruction {
        let (deposit_receipt_pda, _) =
            find_deposit_receipt_pda(&program_id, &session_pda, &recipient);

        let ix = PortalInstruction::DepositFee { lamports };
        let data = borsh::to_vec(&ix).unwrap();
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(depositor, true),
                AccountMeta::new_readonly(session_pda, false),
                AccountMeta::new(deposit_receipt_pda, false),
                AccountMeta::new_readonly(recipient, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data,
        }
    }

    fn build_delegate_ix(
        program_id: Pubkey,
        payer: Pubkey,
        delegated_account: Pubkey,
        owner_program: Pubkey,
        delegation_record_pda: Pubkey,
        buffer: Pubkey,
        session_pda: Pubkey,
        grid_id: u64,
    ) -> Instruction {
        let ix = PortalInstruction::Delegate { grid_id };
        let data = borsh::to_vec(&ix).unwrap();
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(payer, true),
                AccountMeta::new_readonly(system_program::id(), false),
                AccountMeta::new_readonly(session_pda, false),
                AccountMeta::new(delegated_account, true),
                AccountMeta::new_readonly(owner_program, false),
                AccountMeta::new(delegation_record_pda, false),
                AccountMeta::new_readonly(buffer, false),
            ],
            data,
        }
    }

    fn create_test_cluster_info() -> Arc<ClusterInfo> {
        let keypair = Arc::new(Keypair::new());
        let contact_info =
            ContactInfo::new_localhost(&keypair.pubkey(), solana_time_utils::timestamp());
        Arc::new(ClusterInfo::new(
            contact_info,
            keypair,
            SocketAddrSpace::Unspecified,
        ))
    }

    fn find_free_addr() -> SocketAddr {
        crate::ephemeral_runtime::find_free_test_addr()
    }

    fn wait_for_rpc_ready(rpc_client: &RpcClient) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if rpc_client.get_latest_blockhash().is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            rpc_client.get_latest_blockhash().is_ok(),
            "timed out waiting for RPC to accept requests"
        );
    }

    /// Test: Deploy portal BPF program and execute OpenSession -> verify L1 event detection
    #[test]
    fn test_e2e_portal_to_l1_events() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let grid_id = 1u64;
        let ttl_slots = 1000u64;
        let fee_cap = 5_000_000_000u64;
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);

        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            ttl_slots,
            fee_cap,
        );

        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );

        let result = bank.process_transaction(&tx);
        assert!(result.is_ok(), "OpenSession should succeed: {:?}", result);

        let bank_ref = bank;

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
            checkpoint_plan_dir: None,
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        let session_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, L1Event::SessionOpened { .. }))
            .collect();
        assert_eq!(
            session_events.len(),
            1,
            "Should detect exactly one SessionOpened event"
        );

        if let L1Event::SessionOpened {
            session_pda: _,
            grid_id: detected_grid_id,
            ttl_slots: detected_ttl,
            fee_cap: detected_fee,
        } = session_events[0]
        {
            assert_eq!(*detected_grid_id, grid_id, "Grid ID should match");
            assert_eq!(*detected_ttl, ttl_slots, "TTL should match");
            assert_eq!(*detected_fee, fee_cap, "Fee cap should match");
        } else {
            panic!("Expected SessionOpened event");
        }
    }

    /// Test: Execute Delegate instruction and verify AccountDelegated event
    #[test]
    fn test_e2e_delegation_detected() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let owner_program = Pubkey::new_unique();
        let delegated_keypair = Keypair::new();
        let delegated_account = delegated_keypair.pubkey();
        let portal_owned_account = AccountSharedData::new(1_000_000, 100, &program_id);
        bank.store_account(&delegated_account, &portal_owned_account);

        let buffer = Pubkey::new_unique();
        let buffer_account = AccountSharedData::new(1_000_000, 100, &owner_program);
        bank.store_account(&buffer, &buffer_account);

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);
        let (delegation_record_pda, _) =
            find_delegation_record_pda(&program_id, &delegated_account);

        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        bank.process_transaction(&tx).unwrap();

        let delegate_ix = build_delegate_ix(
            program_id,
            owner_pubkey,
            delegated_account,
            owner_program,
            delegation_record_pda,
            buffer,
            session_pda,
            grid_id,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[delegate_ix],
            Some(&owner_pubkey),
            &[&owner_keypair, &delegated_keypair],
            blockhash,
        );
        let result = bank.process_transaction(&tx);
        assert!(result.is_ok(), "Delegate should succeed: {:?}", result);

        let bank_ref = bank;

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
            checkpoint_plan_dir: None,
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        let delegation_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, L1Event::AccountDelegated { .. }))
            .collect();
        assert_eq!(
            delegation_events.len(),
            1,
            "Should detect exactly one AccountDelegated event"
        );

        if let L1Event::AccountDelegated {
            delegation_record_pda: _,
            delegated_account: detected_delegated,
            owner_program: detected_owner_program,
            grid_id: detected_grid_id,
        } = delegation_events[0]
        {
            assert_eq!(
                *detected_delegated, delegated_account,
                "Delegated account should match"
            );
            assert_eq!(
                *detected_owner_program, owner_program,
                "Owner program should match"
            );
            assert_eq!(*detected_grid_id, grid_id, "Grid ID should match");
        } else {
            panic!("Expected AccountDelegated event");
        }
    }

    /// Test: Full vertical slice - portal execution -> event detection -> ephemeral runtime -> account visibility
    #[test]
    fn test_e2e_delegated_account_visible_on_l2() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let owner_program = Pubkey::new_unique();
        let delegated_keypair = Keypair::new();
        let delegated_account = delegated_keypair.pubkey();
        let delegated_account_data = (0..100).map(|i| (i as u8) ^ 0xAB).collect::<Vec<_>>();

        // Pre-stage delegated_account: Portal-owned, zero data (post-buffer-dance).
        let portal_owned_account =
            AccountSharedData::new(1_000_000, delegated_account_data.len(), &program_id);
        bank.store_account(&delegated_account, &portal_owned_account);

        // Pre-stage buffer: owner_program-owned, holding the data Portal will copy back
        // into delegated_account.
        let buffer = Pubkey::new_unique();
        let mut buffer_account =
            AccountSharedData::new(1_000_000, delegated_account_data.len(), &owner_program);
        buffer_account
            .data_as_mut_slice()
            .copy_from_slice(&delegated_account_data);
        bank.store_account(&buffer, &buffer_account);

        let buffer_snapshot_before_delegate = bank
            .get_account(&buffer)
            .expect("buffer should exist on L1 before delegate tx");
        assert_eq!(
            buffer_snapshot_before_delegate.data(),
            delegated_account_data.as_slice(),
            "buffer should hold the data Portal will install into delegated_account"
        );

        let grid_id = 1u64;
        let ttl_slots = 1000u64;
        let fee_cap = 5_000_000_000u64;
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);
        let (delegation_record_pda, _) =
            find_delegation_record_pda(&program_id, &delegated_account);

        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            ttl_slots,
            fee_cap,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        bank.process_transaction(&tx).unwrap();

        let delegate_ix = build_delegate_ix(
            program_id,
            owner_pubkey,
            delegated_account,
            owner_program,
            delegation_record_pda,
            buffer,
            session_pda,
            grid_id,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[delegate_ix],
            Some(&owner_pubkey),
            &[&owner_keypair, &delegated_keypair],
            blockhash,
        );
        bank.process_transaction(&tx).unwrap();
        bank.freeze();

        let bank_ref = bank;

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
            checkpoint_plan_dir: None,
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        let session_event = events
            .iter()
            .find(|e| matches!(e, L1Event::SessionOpened { .. }))
            .expect("Should have SessionOpened event");

        let L1Event::SessionOpened {
            session_pda,
            grid_id,
            ttl_slots,
            fee_cap,
        } = session_event
        else {
            panic!("Expected SessionOpened");
        };

        let delegation_event = events
            .iter()
            .find(|e| matches!(e, L1Event::AccountDelegated { .. }))
            .expect("Should have AccountDelegated event");

        let L1Event::AccountDelegated {
            delegated_account,
            owner_program: detected_owner_program,
            ..
        } = delegation_event
        else {
            panic!("Expected AccountDelegated");
        };
        assert_eq!(
            *detected_owner_program, owner_program,
            "delegation event should preserve owner program"
        );

        let parent_bank = Arc::clone(&bank_ref);

        let settings = EphemeralRollupSettings {
            session_pda: *session_pda,
            grid_id: *grid_id,
            ttl_slots: *ttl_slots,
            fee_cap: *fee_cap,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
            delegated_accounts: vec![*delegated_account],
        };

        let cluster_info = create_test_cluster_info();
        let mut runtime = EphemeralRuntime::new(
            parent_bank,
            cluster_info,
            settings,
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
            program_id,
            Arc::new(Keypair::new()),
        )
        .expect("Failed to create ephemeral runtime");

        assert!(
            runtime.delegated_accounts().contains(delegated_account),
            "Delegated account should be in runtime's delegated set"
        );

        let ephemeral_bank = runtime.bank();
        let account_opt = ephemeral_bank.get_account(delegated_account);
        assert!(
            account_opt.is_some(),
            "Delegated account should be readable on L2"
        );

        let account = account_opt.unwrap();
        let account_data = account.data();
        assert_eq!(
            account.owner(),
            &owner_program,
            "ER account owner should be the delegated owner program"
        );
        assert_eq!(
            account_data,
            delegated_account_data.as_slice(),
            "ER account data should match bytes written before delegation"
        );

        let rpc_client = RpcClient::new(runtime.rpc_addr());
        wait_for_rpc_ready(&rpc_client);
        let rpc_account = rpc_client
            .get_account_data(delegated_account)
            .expect("Delegated account should be readable via RPC");
        assert_eq!(
            rpc_account, delegated_account_data,
            "ER RPC should expose same delegated bytes"
        );

        let l1_account = bank_ref
            .get_account(delegated_account)
            .expect("Delegated account should still exist on L1");
        assert_eq!(
            l1_account.owner(),
            &program_id,
            "L1 account owner should still be portal program"
        );
        assert_eq!(
            l1_account.data(),
            delegated_account_data.as_slice(),
            "L1 account data should survive portal delegate instruction"
        );

        runtime.shutdown();
    }

    /// Test: handle_delegation adds a new delegated account to a running ER at runtime
    #[test]
    fn test_e2e_handle_delegation_at_runtime() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let owner_program = Pubkey::new_unique();
        let delegated_account_pubkey = Pubkey::new_unique();
        let delegated_data = vec![0xDE; 64];
        let mut delegated_account =
            AccountSharedData::new(5_000_000_000, delegated_data.len(), &program_id);
        delegated_account
            .data_as_mut_slice()
            .copy_from_slice(&delegated_data);
        bank.store_account(&delegated_account_pubkey, &delegated_account);

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);

        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        bank.process_transaction(&tx).unwrap();
        bank.freeze();

        let parent_bank = bank;
        let cluster_info = create_test_cluster_info();
        let settings = EphemeralRollupSettings {
            session_pda,
            grid_id,
            ttl_slots: 1000,
            fee_cap: 5_000_000_000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
            delegated_accounts: vec![],
        };

        let mut runtime = EphemeralRuntime::new(
            parent_bank.clone(),
            cluster_info,
            settings,
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
            program_id,
            Arc::new(Keypair::new()),
        )
        .expect("Failed to create ephemeral runtime");

        assert!(
            !runtime
                .delegated_accounts()
                .contains(&delegated_account_pubkey),
            "Account should not be delegated yet"
        );

        let account_data = parent_bank
            .get_account(&delegated_account_pubkey)
            .expect("Account should exist on L1");
        runtime.handle_delegation_with_owner_program(
            &delegated_account_pubkey,
            account_data.clone(),
            Some(owner_program),
        );

        assert!(
            runtime
                .delegated_accounts()
                .contains(&delegated_account_pubkey),
            "Account should be delegated after handle_delegation"
        );

        let er_bank = runtime.bank();
        let er_account = er_bank
            .get_account(&delegated_account_pubkey)
            .expect("Delegated account should be readable on ER");
        assert_eq!(
            er_account.data(),
            &delegated_data[..],
            "Account data should match L1 data"
        );
        assert_eq!(
            er_account.owner(),
            &owner_program,
            "ER account owner should be remapped to owner program"
        );
        assert_eq!(er_account.lamports(), 5_000_000_000);

        let rpc_client = RpcClient::new(runtime.rpc_addr());
        wait_for_rpc_ready(&rpc_client);
        let rpc_balance = rpc_client
            .get_balance(&delegated_account_pubkey)
            .expect("Should be able to get balance via RPC");
        assert_eq!(rpc_balance, 5_000_000_000, "RPC balance should match");

        runtime.shutdown();
    }

    #[test]
    fn test_startup_resume_hydrates_delegated_accounts_from_l1_in_progress() {
        setup();

        let (bank, _bank_forks, program_id, _mint_keypair) = setup_bank_with_portal();
        let manager_account = Arc::new(Keypair::new());
        let owner_program = Pubkey::new_unique();
        let delegated_account_pubkey = Pubkey::new_unique();
        let committed_data = vec![0xC0, 0xFF, 0xEE, 0x42];
        let committed_lamports = 5_000_000_000;
        let grid_id = 7;
        let ttl_slots = 1_000;
        let fee_cap = 123_456;
        let (session_pda, session_bump) = find_session_pda(&program_id);

        let session = Session {
            discriminator: Session::DISCRIMINATOR,
            grid_id,
            ttl_slots,
            fee_cap,
            created_at: bank.slot(),
            nonce: 1,
            authority: Pubkey::new_unique(),
            validator: manager_account.pubkey(),
            settlement_interval_slots: 10,
            last_settled_l1_slot: bank.slot(),
            last_settled_er_slot: 55,
            settlement_status: SettlementStatus::InProgress,
            settlement_er_slot: 55,
            settlement_checksum: [9; 32],
            settlement_accumulator: [0; 32],
            settlement_started_l1_slot: 0,
            bump: session_bump,
        };
        let session_data = borsh::to_vec(&session).unwrap();
        let mut session_account =
            AccountSharedData::new(1_000_000, session_data.len(), &program_id);
        session_account
            .data_as_mut_slice()
            .copy_from_slice(&session_data);
        bank.store_account(&session_pda, &session_account);

        let mut delegated_account =
            AccountSharedData::new(committed_lamports, committed_data.len(), &program_id);
        delegated_account
            .data_as_mut_slice()
            .copy_from_slice(&committed_data);
        bank.store_account(&delegated_account_pubkey, &delegated_account);
        store_delegation_record(
            &bank,
            &program_id,
            &delegated_account_pubkey,
            &owner_program,
            grid_id,
        );
        bank.freeze();

        let mut manager = Manager::new(ManagerConfig {
            portal_program_id: program_id,
            manager_account,
            checkpoint_plan_dir: None,
        });
        manager
            .init_runtime(
                bank.clone(),
                create_test_cluster_info(),
                find_free_addr(),
                find_free_addr(),
                find_free_addr(),
            )
            .expect("runtime should initialize");

        assert!(
            manager.resume_active_session_from_l1(bank.clone()),
            "startup should resume active L1 session"
        );
        assert!(manager.has_active_runtime());

        let runtime = manager.runtime.as_ref().expect("runtime should exist");
        assert_eq!(*runtime.session_pda().read().unwrap(), Some(session_pda));
        assert!(runtime
            .delegated_accounts()
            .contains(&delegated_account_pubkey));
        let er_account = runtime
            .bank()
            .get_account(&delegated_account_pubkey)
            .expect("delegated account should be hydrated into ER");
        assert_eq!(er_account.owner(), &owner_program);
        assert_eq!(er_account.data(), committed_data.as_slice());
        assert_eq!(er_account.lamports(), committed_lamports);

        manager.shutdown_runtime();
    }

    #[test]
    fn test_startup_resume_restores_unsettled_er_balance_from_disk() {
        setup();

        let (bank, _bank_forks, program_id, _mint_keypair) = setup_bank_with_portal();
        let manager_account = Arc::new(Keypair::new());
        let depositor = Pubkey::new_unique();
        let grid_id = 7;
        let (session_pda, session_bump) = find_session_pda(&program_id);
        let session = Session {
            discriminator: Session::DISCRIMINATOR,
            grid_id,
            ttl_slots: 1_000,
            fee_cap: 123_456,
            created_at: bank.slot(),
            nonce: 1,
            authority: Pubkey::new_unique(),
            validator: manager_account.pubkey(),
            settlement_interval_slots: 10,
            last_settled_l1_slot: bank.slot(),
            last_settled_er_slot: 0,
            settlement_status: SettlementStatus::Idle,
            settlement_er_slot: 0,
            settlement_checksum: [0; 32],
            settlement_accumulator: [0; 32],
            settlement_started_l1_slot: 0,
            bump: session_bump,
        };
        let session_data = borsh::to_vec(&session).unwrap();
        let mut session_account =
            AccountSharedData::new(1_000_000, session_data.len(), &program_id);
        session_account
            .data_as_mut_slice()
            .copy_from_slice(&session_data);
        bank.store_account(&session_pda, &session_account);
        bank.freeze();

        let state_root = tempfile::TempDir::new().unwrap();
        let config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: manager_account.clone(),
            checkpoint_plan_dir: Some(state_root.path().join("checkpoint-plans")),
        };
        let mut manager = Manager::new(config.clone());
        manager
            .init_runtime(
                bank.clone(),
                create_test_cluster_info(),
                find_free_addr(),
                find_free_addr(),
                find_free_addr(),
            )
            .unwrap();
        assert!(manager.resume_active_session_from_l1(bank.clone()));
        manager.credit_deposit(&depositor, 777);
        assert_eq!(
            manager
                .runtime
                .as_ref()
                .unwrap()
                .bank()
                .get_balance(&depositor),
            777
        );
        manager.shutdown_runtime();

        let mut resumed_manager = Manager::new(config);
        resumed_manager
            .init_runtime(
                bank.clone(),
                create_test_cluster_info(),
                find_free_addr(),
                find_free_addr(),
                find_free_addr(),
            )
            .unwrap();
        assert!(resumed_manager.resume_active_session_from_l1(bank));
        assert_eq!(
            resumed_manager
                .runtime
                .as_ref()
                .unwrap()
                .bank()
                .get_balance(&depositor),
            777,
            "unsettled ER balance must survive validator restart",
        );
        resumed_manager.shutdown_runtime();
    }

    #[test]
    fn test_manager_handle_delegation_infers_owner_program() {
        setup();

        let (bank, _bank_forks, program_id, _mint_keypair) = setup_bank_with_portal();
        let owner_program = Pubkey::new_unique();
        let delegated_account_pubkey = Pubkey::new_unique();
        let delegated_data = vec![0xAB; 32];
        let mut delegated_account =
            AccountSharedData::new(5_000_000_000, delegated_data.len(), &program_id);
        delegated_account
            .data_as_mut_slice()
            .copy_from_slice(&delegated_data);
        bank.store_account(&delegated_account_pubkey, &delegated_account);
        store_delegation_record(
            &bank,
            &program_id,
            &delegated_account_pubkey,
            &owner_program,
            1,
        );
        bank.freeze();

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
            checkpoint_plan_dir: None,
        };
        let mut manager = Manager::new(manager_config);
        let settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 1,
            ttl_slots: 100,
            fee_cap: 1000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
            delegated_accounts: vec![],
        };
        manager
            .create_ephemeral_runtime(
                bank.clone(),
                create_test_cluster_info(),
                settings,
                find_free_addr(),
            )
            .expect("Failed to create ephemeral runtime");

        manager.handle_delegation(&bank, &delegated_account_pubkey);

        let runtime = manager.runtime.as_ref().expect("runtime should exist");
        let er_account = runtime
            .bank()
            .get_account(&delegated_account_pubkey)
            .expect("Delegated account should be readable on ER");
        assert_eq!(er_account.owner(), &owner_program);
        assert_eq!(er_account.data(), &delegated_data[..]);

        manager.shutdown_runtime();
    }

    #[test]
    fn test_token_deposit_receipt_rejects_unregistered_session_bridge() {
        setup();
        let (bank, _bank_forks, portal_program, _mint_keypair) = setup_bank_with_portal();
        let bridge_program = Pubkey::new_unique();
        let session = Pubkey::new_unique();
        let session_bridge = Pubkey::new_unique();
        let er_token_account = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let bridge_state = northstar_portal::SessionBridge {
            discriminator: northstar_portal::SessionBridge::DISCRIMINATOR,
            session,
            mint,
            bridge_program,
            vault: Pubkey::new_unique(),
            token_program: Pubkey::new_unique(),
            bump: 255,
        };
        let bridge_data = borsh::to_vec(&bridge_state).unwrap();
        let mut bridge_account =
            AccountSharedData::new(1_000_000, bridge_data.len(), &portal_program);
        bridge_account
            .data_as_mut_slice()
            .copy_from_slice(&bridge_data);
        bank.store_account(&session_bridge, &bridge_account);
        bank.freeze();

        let deposit_bank = Bank::new_from_parent(bank, SlotLeader::new_unique(), 2);
        let (deposit_receipt, bump) = northstar_token_bridge::find_token_deposit_receipt_pda(
            &bridge_program,
            &session_bridge,
            &er_token_account,
        );
        let receipt_state = northstar_token_bridge::state::TokenDepositReceipt {
            discriminator: northstar_token_bridge::state::TokenDepositReceipt::DISCRIMINATOR,
            session_bridge: session_bridge.to_bytes(),
            er_token_account: er_token_account.to_bytes(),
            balance: 600_000_000,
            withdrawn: 0,
            bump,
        };
        let receipt_data = borsh::to_vec(&receipt_state).unwrap();
        let mut receipt_account =
            AccountSharedData::new(1_000_000, receipt_data.len(), &bridge_program);
        receipt_account
            .data_as_mut_slice()
            .copy_from_slice(&receipt_data);
        deposit_bank.store_account(&deposit_receipt, &receipt_account);
        deposit_bank.freeze();

        let manager = Manager::new(ManagerConfig {
            portal_program_id: portal_program,
            manager_account: Arc::new(Keypair::new()),
            checkpoint_plan_dir: None,
        });
        let events = manager.get_l1_events(&deposit_bank);
        assert!(!events
            .iter()
            .any(|event| matches!(event, L1Event::TokenDeposited { .. })));
    }

    #[test]
    fn test_fee_deposit_rejects_unregistered_receipt() {
        setup();
        let (bank, _bank_forks, portal_program, _mint_keypair) = setup_bank_with_portal();
        bank.freeze();

        let deposit_bank = Bank::new_from_parent(bank, SlotLeader::new_unique(), 2);
        let recipient = Pubkey::new_unique();
        let receipt = northstar_portal::DepositReceipt {
            discriminator: northstar_portal::DepositReceipt::DISCRIMINATOR,
            session: northstar_portal::find_session_pda(&portal_program).0,
            recipient,
            balance: 0,
            withdrawn: 0,
            bump: 255,
        };
        let receipt_data = borsh::to_vec(&receipt).unwrap();
        let lamports = solana_rent::Rent::default()
            .minimum_balance(receipt_data.len())
            .saturating_add(1_000);
        let mut receipt_account =
            AccountSharedData::new(lamports, receipt_data.len(), &portal_program);
        receipt_account
            .data_as_mut_slice()
            .copy_from_slice(&receipt_data);
        deposit_bank.store_account(&Pubkey::new_unique(), &receipt_account);
        deposit_bank.freeze();

        let manager = Manager::new(ManagerConfig {
            portal_program_id: portal_program,
            manager_account: Arc::new(Keypair::new()),
            checkpoint_plan_dir: None,
        });
        let events = manager.get_l1_events(&deposit_bank);

        assert!(!events
            .iter()
            .any(|event| matches!(event, L1Event::FeeDeposited { .. })));
    }

    #[test]
    fn test_session_event_rejects_unregistered_account() {
        setup();
        let (bank, _bank_forks, portal_program, _mint_keypair) = setup_bank_with_portal();
        bank.freeze();

        let event_bank = Bank::new_from_parent(bank, SlotLeader::new_unique(), 2);
        let session = northstar_portal::Session {
            discriminator: northstar_portal::Session::DISCRIMINATOR,
            grid_id: 1,
            ttl_slots: 100,
            fee_cap: 1_000,
            created_at: 1,
            nonce: 0,
            authority: Pubkey::new_unique(),
            validator: Pubkey::new_unique(),
            settlement_interval_slots: 10,
            last_settled_l1_slot: 0,
            last_settled_er_slot: 0,
            settlement_status: northstar_portal::SettlementStatus::Idle,
            settlement_er_slot: 0,
            settlement_checksum: [0; 32],
            settlement_accumulator: [0; 32],
            settlement_started_l1_slot: 0,
            bump: 255,
        };
        let session_data = borsh::to_vec(&session).unwrap();
        let mut session_account =
            AccountSharedData::new(1_000_000, session_data.len(), &portal_program);
        session_account
            .data_as_mut_slice()
            .copy_from_slice(&session_data);
        let unregistered_session = Pubkey::new_unique();
        event_bank.store_account(&unregistered_session, &session_account);
        event_bank.freeze();

        let manager = Manager::new(ManagerConfig {
            portal_program_id: portal_program,
            manager_account: Arc::new(Keypair::new()),
            checkpoint_plan_dir: None,
        });
        let events = manager.get_l1_events(&event_bank);

        assert!(!events
            .iter()
            .any(|event| matches!(event, L1Event::SessionOpened { .. })));

        let close_bank = Bank::new_from_parent(Arc::new(event_bank), SlotLeader::new_unique(), 3);
        assert!(manager
            .parse_zeroed_account(&close_bank, &unregistered_session)
            .is_none());
    }

    #[test]
    fn test_e2e_deposit_fee_detected() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);

        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        bank.process_transaction(&tx).unwrap();

        let deposit_amount = 2_000_000_000u64;
        let deposit_fee_ix = build_deposit_fee_ix(
            program_id,
            owner_pubkey,
            session_pda,
            owner_pubkey,
            deposit_amount,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[deposit_fee_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        let result = bank.process_transaction(&tx);
        assert!(result.is_ok(), "DepositFee should succeed: {:?}", result);

        let bank_ref = bank;

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
            checkpoint_plan_dir: None,
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        let fee_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, L1Event::FeeDeposited { .. }))
            .collect();
        assert!(
            !fee_events.is_empty(),
            "Should detect at least one FeeDeposited event"
        );

        let deposit_event = fee_events.iter().find(|e| {
            if let L1Event::FeeDeposited {
                delta, depositor, ..
            } = e
            {
                *delta == deposit_amount && *depositor == owner_pubkey
            } else {
                false
            }
        });
        assert!(
            deposit_event.is_some(),
            "Should detect the 2 SOL deposit with delta and depositor"
        );

        if let Some(L1Event::FeeDeposited {
            delta,
            depositor,
            amount,
            ..
        }) = deposit_event
        {
            assert_eq!(*delta, deposit_amount, "Delta should equal deposit amount");
            assert_eq!(
                *depositor, owner_pubkey,
                "Depositor should be the vault authority (owner)"
            );
            assert_eq!(
                *amount, deposit_amount,
                "Amount should be total vault balance"
            );
        }
    }

    #[test]
    fn test_settlement_pays_out_er_withdrawal_delta_on_l1() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();
        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);
        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            bank.last_blockhash(),
        );
        bank.process_transaction(&tx).unwrap();

        let deposit_amount = 1_000_000_000u64;
        let deposit_fee_ix = build_deposit_fee_ix(
            program_id,
            owner_pubkey,
            session_pda,
            owner_pubkey,
            deposit_amount,
        );
        let tx = Transaction::new_signed_with_payer(
            &[deposit_fee_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            bank.last_blockhash(),
        );
        bank.process_transaction(&tx).unwrap();
        bank.freeze();

        let l1_recipient = Pubkey::new_unique();
        let validator = owner_keypair;
        let first_settlement_bank = Bank::new_from_parent(
            bank.clone(),
            SlotLeader::default(),
            bank.slot().saturating_add(11),
        );
        let first_plan = build_settlement_plan(
            &ErStateDiff {
                accounts: vec![],
                lt_hash: LtHash::identity(),
            },
            &HashSet::new(),
            7,
            vec![crate::settlement::ReceiptBalanceSettlement {
                er_source: owner_pubkey,
                l1_recipient: owner_pubkey,
                balance: deposit_amount,
                withdrawn: 0,
                payout_lamports: 0,
            }],
        )
        .unwrap();
        store_committed_checkpoint(
            &first_settlement_bank,
            &program_id,
            &session_pda,
            first_plan.er_slot,
            first_plan.checksum,
            &validator.pubkey(),
        );
        let first_tx = Transaction::new_signed_with_payer(
            &first_plan.portal_instructions(program_id, session_pda, validator.pubkey()),
            Some(&validator.pubkey()),
            &[&validator],
            first_settlement_bank.last_blockhash(),
        );
        first_settlement_bank
            .process_transaction(&first_tx)
            .unwrap();
        first_settlement_bank.freeze();

        let withdraw_amount = 250_000_000u64;
        let second_settlement_bank = Bank::new_from_parent(
            Arc::new(first_settlement_bank),
            SlotLeader::default(),
            bank.slot().saturating_add(22),
        );
        let balance_before = second_settlement_bank.get_balance(&l1_recipient);
        let second_plan = build_settlement_plan(
            &ErStateDiff {
                accounts: vec![],
                lt_hash: LtHash::identity(),
            },
            &HashSet::new(),
            8,
            vec![crate::settlement::ReceiptBalanceSettlement {
                er_source: owner_pubkey,
                l1_recipient,
                balance: deposit_amount - withdraw_amount,
                withdrawn: withdraw_amount,
                payout_lamports: withdraw_amount,
            }],
        )
        .unwrap();
        store_committed_checkpoint(
            &second_settlement_bank,
            &program_id,
            &session_pda,
            second_plan.er_slot,
            second_plan.checksum,
            &validator.pubkey(),
        );
        let second_tx = Transaction::new_signed_with_payer(
            &second_plan.portal_instructions(program_id, session_pda, validator.pubkey()),
            Some(&validator.pubkey()),
            &[&validator],
            second_settlement_bank.last_blockhash(),
        );
        second_settlement_bank
            .process_transaction(&second_tx)
            .unwrap();

        let balance_after = second_settlement_bank.get_balance(&l1_recipient);
        assert_eq!(balance_after - balance_before, withdraw_amount);
        assert_ne!(owner_pubkey, l1_recipient);

        let (receipt_pda, _) = find_deposit_receipt_pda(&program_id, &session_pda, &owner_pubkey);
        let receipt_account = second_settlement_bank.get_account(&receipt_pda).unwrap();
        let Some(PortalAccount::DepositReceipt(receipt)) =
            try_parse_raw_portal_account(receipt_account.data())
        else {
            panic!("deposit receipt should deserialize");
        };
        assert_eq!(receipt.balance, deposit_amount - withdraw_amount);
        assert_eq!(
            deposit_receipt_escrow_lamports(
                &second_settlement_bank,
                receipt_account.lamports(),
                receipt_account.data().len(),
            ),
            deposit_amount - withdraw_amount
        );
    }

    #[test]
    fn test_portal_settles_owner_and_net_zero_lamport_changes() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();
        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);
        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            bank.last_blockhash(),
        );
        bank.process_transaction(&tx).unwrap();

        let first_delegated = Pubkey::new_unique();
        let second_delegated = Pubkey::new_unique();
        let old_owner = Pubkey::new_unique();
        let new_owner = Pubkey::new_unique();
        let first_l1_account = AccountSharedData::new(10_000_000, 1, &old_owner);
        let second_l1_account = AccountSharedData::new(20_000_000, 1, &old_owner);
        let first_portal_account = AccountSharedData::new(10_000_000, 1, &program_id);
        let second_portal_account = AccountSharedData::new(20_000_000, 1, &program_id);
        bank.store_account(&first_delegated, &first_portal_account);
        bank.store_account(&second_delegated, &second_portal_account);
        store_delegation_record(&bank, &program_id, &first_delegated, &old_owner, grid_id);
        store_delegation_record(&bank, &program_id, &second_delegated, &old_owner, grid_id);

        bank.freeze();
        let settlement_bank = Bank::new_from_parent(
            bank.clone(),
            SlotLeader::default(),
            bank.slot().saturating_add(11),
        );
        let first_er_account = AccountSharedData::new(9_000_000, 1, &new_owner);
        let second_er_account = AccountSharedData::new(21_000_000, 1, &old_owner);
        let diff = ErStateDiff {
            accounts: vec![
                ErStateDiffAccount {
                    pubkey: first_delegated,
                    l1_account: Some(first_l1_account),
                    er_account: first_er_account,
                    l1_lt_hash: LtHash::identity(),
                    er_lt_hash: LtHash::identity(),
                },
                ErStateDiffAccount {
                    pubkey: second_delegated,
                    l1_account: Some(second_l1_account),
                    er_account: second_er_account,
                    l1_lt_hash: LtHash::identity(),
                    er_lt_hash: LtHash::identity(),
                },
            ],
            lt_hash: LtHash::identity(),
        };
        let delegated_accounts = HashSet::from([first_delegated, second_delegated]);
        let plan = build_settlement_plan(&diff, &delegated_accounts, 7, vec![]).unwrap();
        assert_eq!(plan.owner_changes.len(), 1);
        assert_eq!(plan.lamport_changes.len(), 2);
        store_committed_checkpoint(
            &settlement_bank,
            &program_id,
            &session_pda,
            plan.er_slot,
            plan.checksum,
            &owner_pubkey,
        );

        let transactions = plan.portal_transactions(
            program_id,
            session_pda,
            &owner_keypair,
            settlement_bank.last_blockhash(),
        );
        assert_eq!(transactions.len(), 1);
        settlement_bank
            .process_transaction(&transactions[0])
            .unwrap();

        assert_eq!(settlement_bank.get_balance(&first_delegated), 9_000_000);
        assert_eq!(settlement_bank.get_balance(&second_delegated), 21_000_000);

        let (record_pda, _) = find_delegation_record_pda(&program_id, &first_delegated);
        let record_account = settlement_bank.get_account(&record_pda).unwrap();
        let Some(PortalAccount::DelegationRecord(record)) =
            try_parse_raw_portal_account(record_account.data())
        else {
            panic!("delegation record should deserialize");
        };
        assert_eq!(record.owner_program, new_owner);
    }

    type CheckpointFlowFixture = (
        Arc<Bank>,
        Manager,
        Pubkey,
        Pubkey,
        Pubkey,
        Pubkey,
        Vec<u8>,
        Vec<u8>,
    );

    fn setup_checkpoint_flow_fixture() -> CheckpointFlowFixture {
        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();
        let manager_account = Arc::new(Keypair::new());
        let checkpoint_plan_dir = std::env::temp_dir().join(format!(
            "northstar-checkpoint-plan-test-{}",
            manager_account.pubkey()
        ));
        let _ = std::fs::remove_dir_all(&checkpoint_plan_dir);
        bank.transfer(100_000_000_000, &mint_keypair, &manager_account.pubkey())
            .unwrap();

        let grid_id = 7;
        let settlement_interval_slots = 10;
        let (session_pda, session_bump) = find_session_pda(&program_id);
        store_session(
            &bank,
            &program_id,
            &session_pda,
            session_bump,
            &manager_account.pubkey(),
            grid_id,
            settlement_interval_slots,
        );

        let owner_program = Pubkey::new_unique();
        let delegated_account = Pubkey::new_unique();
        let l1_data = vec![0x10, 0x11, 0x12, 0x13];
        let er_data = vec![0x20, 0x21, 0x22, 0x23];
        let mut delegated_l1 = AccountSharedData::new(1_000_000, l1_data.len(), &program_id);
        delegated_l1.data_as_mut_slice().copy_from_slice(&l1_data);
        bank.store_account(&delegated_account, &delegated_l1);
        store_delegation_record(
            &bank,
            &program_id,
            &delegated_account,
            &owner_program,
            grid_id,
        );
        bank.freeze();

        let mut manager = Manager::new(ManagerConfig {
            portal_program_id: program_id,
            manager_account,
            checkpoint_plan_dir: Some(checkpoint_plan_dir),
        });
        manager
            .create_ephemeral_runtime(
                bank.clone(),
                create_test_cluster_info(),
                EphemeralRollupSettings {
                    session_pda,
                    grid_id,
                    ttl_slots: 1_000,
                    fee_cap: 123_456,
                    er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
                    delegated_accounts: vec![delegated_account],
                },
                find_free_addr(),
            )
            .expect("runtime should start");
        let runtime = manager.runtime.as_mut().unwrap();
        runtime.set_session_pda(session_pda);
        let mut delegated_er = AccountSharedData::new(1_000_000, er_data.len(), &program_id);
        delegated_er.data_as_mut_slice().copy_from_slice(&er_data);
        runtime.handle_delegation_with_owner_program(
            &delegated_account,
            delegated_er,
            Some(owner_program),
        );

        (
            bank,
            manager,
            program_id,
            session_pda,
            delegated_account,
            owner_program,
            l1_data,
            er_data,
        )
    }

    fn first_portal_instruction(transaction: &Transaction) -> PortalInstruction {
        borsh::from_slice(&transaction.message.instructions[0].data).unwrap()
    }

    #[test]
    fn token_withdrawal_release_follows_finish_settlement() {
        let portal_program = Pubkey::new_unique();
        let bridge_program = Pubkey::new_unique();
        let validator = Arc::new(Keypair::new());
        let manager = Manager::new(ManagerConfig {
            portal_program_id: portal_program,
            manager_account: Arc::clone(&validator),
            checkpoint_plan_dir: None,
        });
        let plan = SettlementPlan {
            er_slot: 42,
            checksum: [7; 32],
            chunks: vec![],
            owner_changes: vec![],
            lamport_changes: vec![],
            receipt_balances: vec![],
            token_withdrawals: vec![settlement::TokenWithdrawalSettlement {
                bridge_program,
                session_bridge: Pubkey::new_unique(),
                er_token_account: Pubkey::new_unique(),
                vault: Pubkey::new_unique(),
                vault_token_account: Pubkey::new_unique(),
                l1_destination_token_account: Pubkey::new_unique(),
                mint: Pubkey::new_unique(),
                token_program: Pubkey::new_unique(),
                amount: 10,
                withdrawn: 10,
                decimals: 6,
            }],
            unsupported_changes: vec![],
        };

        let transactions = manager.settlement_transactions_for_plan(
            &plan,
            Pubkey::new_unique(),
            Hash::new_unique(),
            true,
        );
        let program_id = |transaction: &Transaction| {
            let instruction = &transaction.message.instructions[0];
            transaction.message.account_keys[instruction.program_id_index as usize]
        };

        assert_eq!(transactions.len(), 4);
        assert_eq!(program_id(&transactions[2]), portal_program);
        assert_eq!(program_id(&transactions[3]), bridge_program);
    }

    #[test]
    fn validator_checkpoint_flow_waits_then_settles() {
        setup();

        let (
            bank,
            mut manager,
            program_id,
            session_pda,
            delegated_account,
            _owner_program,
            l1_data,
            _er_data,
        ) = setup_checkpoint_flow_fixture();
        let due_slot = bank.slot() + 10;
        let due_bank = Bank::new_from_parent(bank, SlotLeader::default(), due_slot);

        let (er_slot, _checksum, transactions) = manager
            .settlement_transactions_if_due(&due_bank, due_bank.last_blockhash())
            .expect("due diff should propose checkpoint");
        assert_eq!(transactions.len(), 1);
        assert!(matches!(
            first_portal_instruction(&transactions[0]),
            PortalInstruction::ProposeCheckpoint(_)
        ));
        due_bank.process_transaction(&transactions[0]).unwrap();
        assert_eq!(
            due_bank.get_account(&delegated_account).unwrap().data(),
            l1_data.as_slice(),
            "checkpoint proposal must not mutate delegated L1 data",
        );

        let (checkpoint_pda, _) = find_checkpoint_pda(&program_id, &session_pda, er_slot);
        let checkpoint_account = due_bank.get_account(&checkpoint_pda).unwrap();
        let Some(PortalAccount::Checkpoint(checkpoint)) =
            try_parse_raw_portal_account(checkpoint_account.data())
        else {
            panic!("checkpoint should deserialize");
        };
        assert_eq!(checkpoint.status, CheckpointStatus::Pending);
        let checkpoint_plan_path = manager.checkpoint_plan_path(
            session_pda,
            manager.config.manager_account.pubkey(),
            er_slot,
        );
        assert!(
            checkpoint_plan_path.exists(),
            "checkpoint proposal should persist durable settlement plan"
        );

        due_bank.freeze();
        let due_bank = Arc::new(due_bank);
        let manager_config = manager.config.clone();
        manager.shutdown_runtime();
        let mut resumed_manager = Manager::new(manager_config);
        resumed_manager
            .create_ephemeral_runtime(
                due_bank.clone(),
                create_test_cluster_info(),
                EphemeralRollupSettings {
                    session_pda,
                    grid_id: 0,
                    ttl_slots: 0,
                    fee_cap: 0,
                    er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
                    delegated_accounts: vec![],
                },
                find_free_addr(),
            )
            .expect("runtime should restart");
        resumed_manager.deactivate_session();
        assert!(
            resumed_manager.resume_active_session_from_l1(due_bank.clone()),
            "restarted manager should resume active session from L1"
        );
        manager = resumed_manager;

        let wait_bank = Bank::new_from_parent(
            due_bank,
            SlotLeader::default(),
            checkpoint.challenge_deadline_l1_slot - 1,
        );
        assert!(
            manager
                .settlement_transactions_if_due(&wait_bank, wait_bank.last_blockhash())
                .is_none(),
            "pending checkpoint should not settle before deadline",
        );
        let challenge_ix = build_challenge_checkpoint_ix(
            program_id,
            manager.config.manager_account.pubkey(),
            session_pda,
            er_slot,
        );
        let challenge_tx = Transaction::new_signed_with_payer(
            &[challenge_ix],
            Some(&manager.config.manager_account.pubkey()),
            &[manager.config.manager_account.as_ref()],
            wait_bank.last_blockhash(),
        );
        wait_bank.process_transaction(&challenge_tx).unwrap();
        let checkpoint_account = wait_bank.get_account(&checkpoint_pda).unwrap();
        let Some(PortalAccount::Checkpoint(challenged_checkpoint)) =
            try_parse_raw_portal_account(checkpoint_account.data())
        else {
            panic!("checkpoint should deserialize");
        };
        assert_eq!(challenged_checkpoint.status, CheckpointStatus::Challenged);

        wait_bank.freeze();
        let original_deadline_bank = Bank::new_from_parent(
            Arc::new(wait_bank),
            SlotLeader::default(),
            checkpoint.challenge_deadline_l1_slot,
        );
        assert!(
            manager
                .settlement_transactions_if_due(
                    &original_deadline_bank,
                    original_deadline_bank.last_blockhash(),
                )
                .is_none(),
            "challenged checkpoint should not settle at the original proposal deadline",
        );

        original_deadline_bank.freeze();
        let expired_bank = Bank::new_from_parent(
            Arc::new(original_deadline_bank),
            SlotLeader::default(),
            challenged_checkpoint.challenge_deadline_l1_slot + 1,
        );
        assert!(
            manager
                .settlement_transactions_if_due(&expired_bank, expired_bank.last_blockhash())
                .is_none(),
            "challenged checkpoint must remain blocked until explicit resolution"
        );
        assert!(
            checkpoint_plan_path.exists(),
            "unresolved checkpoint must retain durable settlement plan"
        );
        manager.shutdown_runtime();

        let (
            mismatch_bank,
            mut mismatch_manager,
            mismatch_program_id,
            mismatch_session_pda,
            mismatch_delegated,
            mismatch_owner_program,
            mismatch_l1_data,
            mismatch_er_data,
        ) = setup_checkpoint_flow_fixture();
        let mismatch_due_slot = mismatch_bank.slot() + 10;
        let mismatch_due_bank =
            Bank::new_from_parent(mismatch_bank, SlotLeader::default(), mismatch_due_slot);
        let (mismatch_er_slot, _checksum, mismatch_transactions) = mismatch_manager
            .settlement_transactions_if_due(&mismatch_due_bank, mismatch_due_bank.last_blockhash())
            .expect("due diff should propose checkpoint");
        mismatch_due_bank
            .process_transaction(&mismatch_transactions[0])
            .unwrap();

        let runtime = mismatch_manager.runtime.as_ref().unwrap();
        let mut tampered_er = AccountSharedData::new(1_000_000, 4, &mismatch_program_id);
        tampered_er
            .data_as_mut_slice()
            .copy_from_slice(&[0x30, 0x31, 0x32, 0x33]);
        runtime.handle_delegation_with_owner_program(
            &mismatch_delegated,
            tampered_er,
            Some(mismatch_owner_program),
        );

        let (mismatch_checkpoint_pda, _) = find_checkpoint_pda(
            &mismatch_program_id,
            &mismatch_session_pda,
            mismatch_er_slot,
        );
        let mismatch_checkpoint_account = mismatch_due_bank
            .get_account(&mismatch_checkpoint_pda)
            .unwrap();
        let Some(PortalAccount::Checkpoint(mismatch_checkpoint)) =
            try_parse_raw_portal_account(mismatch_checkpoint_account.data())
        else {
            panic!("checkpoint should deserialize");
        };
        mismatch_due_bank.freeze();
        let mismatch_expired_bank = Bank::new_from_parent(
            Arc::new(mismatch_due_bank),
            SlotLeader::default(),
            mismatch_checkpoint.challenge_deadline_l1_slot,
        );
        let (_er_slot, _checksum, transactions) = mismatch_manager
            .settlement_transactions_if_due(
                &mismatch_expired_bank,
                mismatch_expired_bank.last_blockhash(),
            )
            .expect("expired checkpoint should settle from cached checkpoint diff");
        for transaction in &transactions {
            mismatch_expired_bank
                .process_transaction(transaction)
                .unwrap();
        }
        assert_eq!(
            mismatch_expired_bank
                .get_account(&mismatch_delegated)
                .unwrap()
                .data(),
            mismatch_er_data.as_slice(),
            "settlement must use checkpoint-bound diff, not later live ER mutation",
        );
        assert_ne!(
            mismatch_expired_bank
                .get_account(&mismatch_delegated)
                .unwrap()
                .data(),
            mismatch_l1_data.as_slice(),
        );
        mismatch_manager.shutdown_runtime();

        let (
            tamper_bank,
            mut tamper_manager,
            tamper_program_id,
            tamper_session_pda,
            tamper_delegated,
            _tamper_owner_program,
            tamper_l1_data,
            _tamper_er_data,
        ) = setup_checkpoint_flow_fixture();
        let tamper_due_bank = Bank::new_from_parent(
            tamper_bank,
            SlotLeader::default(),
            expired_bank.slot().saturating_add(10),
        );
        let (tamper_er_slot, _checksum, tamper_transactions) = tamper_manager
            .settlement_transactions_if_due(&tamper_due_bank, tamper_due_bank.last_blockhash())
            .expect("due diff should propose checkpoint");
        tamper_due_bank
            .process_transaction(&tamper_transactions[0])
            .unwrap();
        let tamper_plan_path = tamper_manager.checkpoint_plan_path(
            tamper_session_pda,
            tamper_manager.config.manager_account.pubkey(),
            tamper_er_slot,
        );
        let mut durable_plan =
            borsh::from_slice::<DurableSettlementPlan>(&std::fs::read(&tamper_plan_path).unwrap())
                .unwrap();
        durable_plan.chunks[0].data[0] ^= 0xFF;
        std::fs::write(&tamper_plan_path, borsh::to_vec(&durable_plan).unwrap()).unwrap();

        let (tamper_checkpoint_pda, _) =
            find_checkpoint_pda(&tamper_program_id, &tamper_session_pda, tamper_er_slot);
        let tamper_checkpoint_account =
            tamper_due_bank.get_account(&tamper_checkpoint_pda).unwrap();
        let Some(PortalAccount::Checkpoint(tamper_checkpoint)) =
            try_parse_raw_portal_account(tamper_checkpoint_account.data())
        else {
            panic!("checkpoint should deserialize");
        };
        tamper_due_bank.freeze();
        let tamper_due_bank = Arc::new(tamper_due_bank);
        let tamper_manager_config = tamper_manager.config.clone();
        tamper_manager.shutdown_runtime();
        let mut tamper_resumed_manager = Manager::new(tamper_manager_config);
        tamper_resumed_manager
            .create_ephemeral_runtime(
                tamper_due_bank.clone(),
                create_test_cluster_info(),
                EphemeralRollupSettings {
                    session_pda: tamper_session_pda,
                    grid_id: 0,
                    ttl_slots: 0,
                    fee_cap: 0,
                    er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
                    delegated_accounts: vec![],
                },
                find_free_addr(),
            )
            .expect("runtime should restart");
        tamper_resumed_manager.deactivate_session();
        assert!(tamper_resumed_manager.resume_active_session_from_l1(tamper_due_bank.clone()));
        let tamper_expired_bank = Bank::new_from_parent(
            tamper_due_bank,
            SlotLeader::default(),
            tamper_checkpoint.challenge_deadline_l1_slot,
        );
        assert!(
            tamper_resumed_manager
                .settlement_transactions_if_due(
                    &tamper_expired_bank,
                    tamper_expired_bank.last_blockhash(),
                )
                .is_none(),
            "tampered durable plan must not produce settlement transactions"
        );
        assert_eq!(
            tamper_expired_bank
                .get_account(&tamper_delegated)
                .unwrap()
                .data(),
            tamper_l1_data.as_slice(),
        );
        assert!(
            !tamper_plan_path.exists(),
            "tampered durable plan should be quarantined by deletion"
        );
        tamper_resumed_manager.shutdown_runtime();
    }

    #[test]
    fn validator_settlement_requires_finalized_checkpoint() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();
        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        let committer_keypair = Keypair::new();
        let committer_pubkey = committer_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();
        bank.transfer(1_000_000_000, &mint_keypair, &committer_pubkey)
            .unwrap();

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);
        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            bank.last_blockhash(),
        );
        bank.process_transaction(&tx).unwrap();

        let delegated_account = Pubkey::new_unique();
        let owner_program = Pubkey::new_unique();
        let l1_data = vec![0, 0, 0, 0];
        let er_data = vec![1, 2, 3, 4];
        let mut l1_account = AccountSharedData::new(1_000_000, l1_data.len(), &program_id);
        l1_account.data_as_mut_slice().copy_from_slice(&l1_data);
        bank.store_account(&delegated_account, &l1_account);
        store_delegation_record(
            &bank,
            &program_id,
            &delegated_account,
            &owner_program,
            grid_id,
        );

        bank.freeze();
        let settlement_bank = Bank::new_from_parent(
            bank.clone(),
            SlotLeader::default(),
            bank.slot().saturating_add(11),
        );
        let mut er_account = AccountSharedData::new(1_000_000, er_data.len(), &program_id);
        er_account.data_as_mut_slice().copy_from_slice(&er_data);
        let diff = ErStateDiff {
            accounts: vec![ErStateDiffAccount {
                pubkey: delegated_account,
                l1_account: Some(l1_account),
                er_account,
                l1_lt_hash: LtHash::identity(),
                er_lt_hash: LtHash::identity(),
            }],
            lt_hash: LtHash::identity(),
        };
        let delegated_accounts = HashSet::from([delegated_account]);
        let plan = build_settlement_plan(&diff, &delegated_accounts, 7, vec![]).unwrap();

        let propose_ix = build_propose_checkpoint_ix(
            program_id,
            owner_pubkey,
            session_pda,
            plan.er_slot,
            plan.checksum,
            2,
        );
        let propose_tx = Transaction::new_signed_with_payer(
            &[propose_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            settlement_bank.last_blockhash(),
        );
        settlement_bank.process_transaction(&propose_tx).unwrap();

        let early_instructions = plan.portal_instructions(program_id, session_pda, owner_pubkey);
        let early_tx = Transaction::new_signed_with_payer(
            &early_instructions[..1],
            Some(&owner_pubkey),
            &[&owner_keypair],
            settlement_bank.last_blockhash(),
        );
        assert!(settlement_bank.process_transaction(&early_tx).is_err());
        let delegated_after_early = settlement_bank.get_account(&delegated_account).unwrap();
        assert_eq!(delegated_after_early.data(), l1_data.as_slice());

        settlement_bank.freeze();
        let finalized_bank = Bank::new_from_parent(
            Arc::new(settlement_bank),
            SlotLeader::default(),
            bank.slot().saturating_add(14),
        );
        let commit_ix = build_commit_checkpoint_ix(
            program_id,
            committer_pubkey,
            owner_pubkey,
            session_pda,
            plan.er_slot,
        );
        let commit_tx = Transaction::new_signed_with_payer(
            &[commit_ix],
            Some(&committer_pubkey),
            &[&committer_keypair],
            finalized_bank.last_blockhash(),
        );
        finalized_bank.process_transaction(&commit_tx).unwrap();

        let transactions = plan.portal_transactions(
            program_id,
            session_pda,
            &owner_keypair,
            finalized_bank.last_blockhash(),
        );
        assert_eq!(transactions.len(), 1);
        finalized_bank
            .process_transaction(&transactions[0])
            .unwrap();

        let delegated_after = finalized_bank.get_account(&delegated_account).unwrap();
        assert_eq!(delegated_after.data(), er_data.as_slice());
        let session_account = finalized_bank.get_account(&session_pda).unwrap();
        let Some(PortalAccount::Session(session)) =
            try_parse_raw_portal_account(session_account.data())
        else {
            panic!("session should deserialize");
        };
        assert_eq!(session.last_settled_er_slot, plan.er_slot);

        let (checkpoint_pda, _) = find_checkpoint_pda(&program_id, &session_pda, plan.er_slot);
        let checkpoint_account = finalized_bank.get_account(&checkpoint_pda).unwrap();
        let Some(PortalAccount::Checkpoint(checkpoint)) =
            try_parse_raw_portal_account(checkpoint_account.data())
        else {
            panic!("checkpoint should deserialize");
        };
        assert_eq!(checkpoint.status, CheckpointStatus::Settled);

        finalized_bank.freeze();
        let reuse_bank = Bank::new_from_parent(
            Arc::new(finalized_bank),
            SlotLeader::default(),
            bank.slot().saturating_add(15),
        );
        let reuse_transactions = plan.portal_transactions(
            program_id,
            session_pda,
            &owner_keypair,
            reuse_bank.last_blockhash(),
        );
        assert_eq!(reuse_transactions.len(), 1);
        assert!(reuse_bank
            .process_transaction(&reuse_transactions[0])
            .is_err());
    }

    #[test]
    fn test_portal_rejects_settlement_chunk_not_matching_checksum() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();
        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);
        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            bank.last_blockhash(),
        );
        bank.process_transaction(&tx).unwrap();

        let delegated_account = Pubkey::new_unique();
        let owner_program = Pubkey::new_unique();
        let l1_data = vec![0, 0, 0, 0];
        let er_data = vec![1, 2, 3, 4];
        let mut l1_account = AccountSharedData::new(1_000_000, l1_data.len(), &program_id);
        l1_account.data_as_mut_slice().copy_from_slice(&l1_data);
        bank.store_account(&delegated_account, &l1_account);
        store_delegation_record(
            &bank,
            &program_id,
            &delegated_account,
            &owner_program,
            grid_id,
        );

        bank.freeze();
        let settlement_bank = Bank::new_from_parent(
            bank.clone(),
            SlotLeader::default(),
            bank.slot().saturating_add(11),
        );
        let mut er_account = AccountSharedData::new(1_000_000, er_data.len(), &program_id);
        er_account.data_as_mut_slice().copy_from_slice(&er_data);
        let diff = ErStateDiff {
            accounts: vec![ErStateDiffAccount {
                pubkey: delegated_account,
                l1_account: Some(l1_account.clone()),
                er_account,
                l1_lt_hash: LtHash::identity(),
                er_lt_hash: LtHash::identity(),
            }],
            lt_hash: LtHash::identity(),
        };
        let delegated_accounts = HashSet::from([delegated_account]);
        let mut plan = build_settlement_plan(&diff, &delegated_accounts, 7, vec![]).unwrap();
        store_committed_checkpoint(
            &settlement_bank,
            &program_id,
            &session_pda,
            plan.er_slot,
            plan.checksum,
            &owner_pubkey,
        );
        plan.chunks[0].data[0] ^= 0xff;

        let transactions = plan.portal_transactions(
            program_id,
            session_pda,
            &owner_keypair,
            settlement_bank.last_blockhash(),
        );
        assert_eq!(transactions.len(), 1);
        let result = settlement_bank.process_transaction(&transactions[0]);
        assert!(
            result.is_err(),
            "tampered settlement data must fail checksum verification"
        );

        let delegated_after = settlement_bank.get_account(&delegated_account).unwrap();
        assert_eq!(delegated_after.data(), l1_data.as_slice());
    }

    #[test]
    fn test_portal_settlement_retry_after_applied_ops_is_idempotent() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();
        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);
        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let open_tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            bank.last_blockhash(),
        );
        bank.process_transaction(&open_tx).unwrap();

        let deposit_amount = 1_000_000_000u64;
        let settled_receipt_balance = 900_000_000u64;
        let deposit_fee_ix = build_deposit_fee_ix(
            program_id,
            owner_pubkey,
            session_pda,
            owner_pubkey,
            deposit_amount,
        );
        let deposit_tx = Transaction::new_signed_with_payer(
            &[deposit_fee_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            bank.last_blockhash(),
        );
        bank.process_transaction(&deposit_tx).unwrap();

        let delegated_account = Pubkey::new_unique();
        let owner_program = Pubkey::new_unique();
        let l1_data = vec![0, 0, 0];
        let er_data = vec![1, 0, 2];
        let mut l1_account = AccountSharedData::new(1_000_000, l1_data.len(), &program_id);
        l1_account.data_as_mut_slice().copy_from_slice(&l1_data);
        bank.store_account(&delegated_account, &l1_account);
        store_delegation_record(
            &bank,
            &program_id,
            &delegated_account,
            &owner_program,
            grid_id,
        );

        bank.freeze();
        let settlement_bank = Bank::new_from_parent(
            bank.clone(),
            SlotLeader::default(),
            bank.slot().saturating_add(11),
        );
        let mut er_account = AccountSharedData::new(1_000_000, er_data.len(), &program_id);
        er_account.data_as_mut_slice().copy_from_slice(&er_data);
        let diff = ErStateDiff {
            accounts: vec![ErStateDiffAccount {
                pubkey: delegated_account,
                l1_account: Some(l1_account),
                er_account,
                l1_lt_hash: LtHash::identity(),
                er_lt_hash: LtHash::identity(),
            }],
            lt_hash: LtHash::identity(),
        };
        let delegated_accounts = HashSet::from([delegated_account]);
        let plan = build_settlement_plan(
            &diff,
            &delegated_accounts,
            7,
            vec![crate::settlement::ReceiptBalanceSettlement {
                er_source: owner_pubkey,
                l1_recipient: owner_pubkey,
                balance: settled_receipt_balance,
                withdrawn: 0,
                payout_lamports: 0,
            }],
        )
        .unwrap();
        assert_eq!(plan.chunks.len(), 2);
        assert_eq!(plan.receipt_balances.len(), 1);
        store_committed_checkpoint(
            &settlement_bank,
            &program_id,
            &session_pda,
            plan.er_slot,
            plan.checksum,
            &owner_pubkey,
        );
        let instructions = plan.portal_instructions(program_id, session_pda, owner_pubkey);
        assert_eq!(instructions.len(), 5);

        let first_tx = Transaction::new_signed_with_payer(
            &instructions[..instructions.len() - 1],
            Some(&owner_pubkey),
            &[&owner_keypair],
            settlement_bank.last_blockhash(),
        );
        settlement_bank.process_transaction(&first_tx).unwrap();

        let retry_tx = Transaction::new_signed_with_payer(
            &instructions[1..],
            Some(&owner_pubkey),
            &[&owner_keypair],
            settlement_bank.last_blockhash(),
        );
        settlement_bank.process_transaction(&retry_tx).unwrap();

        let delegated_after = settlement_bank.get_account(&delegated_account).unwrap();
        assert_eq!(delegated_after.data(), er_data.as_slice());

        let (receipt_pda, _) = find_deposit_receipt_pda(&program_id, &session_pda, &owner_pubkey);
        let receipt_account = settlement_bank.get_account(&receipt_pda).unwrap();
        let Some(PortalAccount::DepositReceipt(receipt)) =
            try_parse_raw_portal_account(receipt_account.data())
        else {
            panic!("deposit receipt should deserialize");
        };
        assert_eq!(receipt.balance, settled_receipt_balance);
    }

    /// Test: No portal events when there's no portal activity
    #[test]
    fn test_e2e_no_events_without_portal_activity() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let sender_keypair = Keypair::new();
        let sender_pubkey = sender_keypair.pubkey();
        let receiver_pubkey = Pubkey::new_unique();
        bank.transfer(100_000_000_000, &mint_keypair, &sender_pubkey)
            .unwrap();

        let transfer_ix = transfer(&sender_pubkey, &receiver_pubkey, 1_000_000_000);
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[transfer_ix],
            Some(&sender_pubkey),
            &[&sender_keypair],
            blockhash,
        );
        bank.process_transaction(&tx).unwrap();

        let bank_ref = bank;

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
            checkpoint_plan_dir: None,
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        assert!(
            events.is_empty(),
            "Should detect no portal events when there's no portal activity"
        );
    }

    /// Test: Third party deposits to a FeeVault -> verify FeeDeposited event
    #[test]
    fn test_e2e_third_party_deposit_detected() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let depositor_keypair = Keypair::new();
        let depositor_pubkey = depositor_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &depositor_pubkey)
            .unwrap();

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);

        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        bank.process_transaction(&tx).unwrap();

        let deposit_amount = 3_000_000_000u64;
        let deposit_fee_ix = build_deposit_fee_ix(
            program_id,
            depositor_pubkey,
            session_pda,
            depositor_pubkey,
            deposit_amount,
        );
        let blockhash = bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[deposit_fee_ix],
            Some(&depositor_pubkey),
            &[&depositor_keypair],
            blockhash,
        );
        let result = bank.process_transaction(&tx);
        assert!(
            result.is_ok(),
            "Third party DepositFee should succeed: {:?}",
            result
        );

        let bank_ref = bank;

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
            checkpoint_plan_dir: None,
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        let fee_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, L1Event::FeeDeposited { .. }))
            .collect();
        assert!(
            !fee_events.is_empty(),
            "Should detect at least one FeeDeposited event"
        );

        let deposit_event = fee_events.iter().find(|e| {
            if let L1Event::FeeDeposited {
                delta, depositor, ..
            } = e
            {
                *delta == deposit_amount && *depositor == depositor_pubkey
            } else {
                false
            }
        });
        assert!(
            deposit_event.is_some(),
            "Should detect the 3 SOL third party deposit with correct delta and depositor"
        );

        if let Some(L1Event::FeeDeposited { delta, .. }) = deposit_event {
            assert_eq!(
                *delta, deposit_amount,
                "Delta should equal deposit amount (not cumulative)"
            );
        }
    }

    /// Test: Multiple deposits across slots - verify delta is incremental, not cumulative
    #[test]
    fn test_e2e_deposit_delta_computed_correctly() {
        setup();

        let (bank, _bank_forks, program_id, mint_keypair) = setup_bank_with_portal();

        let owner_keypair = Keypair::new();
        let owner_pubkey = owner_keypair.pubkey();
        bank.transfer(100_000_000_000, &mint_keypair, &owner_pubkey)
            .unwrap();

        let bank_slot = bank.slot();
        bank.freeze();
        let child_bank = Bank::new_from_parent(bank, SlotLeader::default(), bank_slot + 1);

        let grid_id = 1u64;
        let (session_pda, _) = find_session_pda(&program_id);
        let (fee_vault_pda, _) = find_fee_vault_pda(&program_id);

        let open_session_ix = build_open_session_ix(
            program_id,
            owner_pubkey,
            session_pda,
            fee_vault_pda,
            grid_id,
            1000,
            5_000_000_000,
        );
        let blockhash = child_bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[open_session_ix],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        child_bank.process_transaction(&tx).unwrap();

        let deposit1_amount = 2_000_000_000u64;
        let deposit_fee_ix1 = build_deposit_fee_ix(
            program_id,
            owner_pubkey,
            session_pda,
            owner_pubkey,
            deposit1_amount,
        );
        let blockhash = child_bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[deposit_fee_ix1],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        child_bank.process_transaction(&tx).unwrap();

        child_bank.freeze();
        let child_bank =
            Bank::new_from_parent(Arc::new(child_bank), SlotLeader::default(), bank_slot + 2);

        let deposit2_amount = 3_000_000_000u64;
        let deposit_fee_ix2 = build_deposit_fee_ix(
            program_id,
            owner_pubkey,
            session_pda,
            owner_pubkey,
            deposit2_amount,
        );
        let blockhash = child_bank.last_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[deposit_fee_ix2],
            Some(&owner_pubkey),
            &[&owner_keypair],
            blockhash,
        );
        child_bank.process_transaction(&tx).unwrap();

        let bank_forks = BankForks::new_rw_arc(child_bank);
        let bank_ref = bank_forks.read().unwrap().root_bank();

        let manager_config = ManagerConfig {
            portal_program_id: program_id,
            manager_account: Arc::new(Keypair::new()),
            checkpoint_plan_dir: None,
        };
        let manager = Manager::new(manager_config);

        let events = manager.get_l1_events(&bank_ref);

        let fee_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, L1Event::FeeDeposited { .. }))
            .collect();

        assert!(
            !fee_events.is_empty(),
            "Should detect at least one FeeDeposited event"
        );

        let second_deposit_event = fee_events.iter().find(|e| {
            if let L1Event::FeeDeposited { delta, amount, .. } = e {
                *delta == deposit2_amount && *amount == (deposit1_amount + deposit2_amount)
            } else {
                false
            }
        });

        assert!(
            second_deposit_event.is_some(),
            "Should detect delta as 3 SOL (not 5 SOL cumulative)"
        );
    }
}

/// Regression coverage for the AccountsBackgroundService panic in
/// `purge_slot_cache_pubkeys` that devnet hit on Apr 17 2026:
///
/// ```text
/// thread 'solAcctsBgSvc' panicked at accounts-db/src/accounts_db.rs:4229:9:
/// assertion failed: self.storage
///     .get_slot_storage_entry_shrinking_in_progress_ok(purged_slot)
///     .is_none()
/// ```
///
/// The root cause is ER + L1 sharing a single `AccountsDb`. If the ER ever
/// commits accounts at a slot the L1 later roots, the L1's background
/// flush trips the invariant. The fix is twofold:
///
/// 1. ER banks are constructed via `Bank::new_from_parent_ephemeral`, which
///    suppresses all epoch-boundary side effects that would poison shared
///    consensus state.
/// 2. `EphemeralRuntime::er_slot_for` places ER banks at `parent.slot + 2^40`
///    so the L1 cannot realistically catch up.
///
/// This test runs a real `AccountsBackgroundService` against an L1
/// `BankForks` while an `EphemeralRuntime` commits transactions on the same
/// `AccountsDb`, advances L1 roots, and asserts ABS stays healthy.
#[cfg(test)]
mod ephemeral_accounts_background_service_regression {
    use {
        super::{ephemeral_runtime::EphemeralRuntime, EphemeralRollupSettings},
        agave_logger::setup,
        agave_snapshots::{snapshot_config::SnapshotConfig, SnapshotInterval},
        crossbeam_channel::unbounded,
        solana_genesis_config::GenesisConfig,
        solana_gossip::{cluster_info::ClusterInfo, contact_info::ContactInfo},
        solana_keypair::Keypair,
        solana_leader_schedule::SlotLeader,
        solana_message::Message,
        solana_net_utils::SocketAddrSpace,
        solana_pubkey::Pubkey,
        solana_runtime::{
            accounts_background_service::{
                AbsRequestHandlers, AccountsBackgroundService, PendingSnapshotPackages,
                PrunedBanksRequestHandler, SendDroppedBankCallback, SnapshotRequestHandler,
            },
            bank::Bank,
            snapshot_controller::SnapshotController,
        },
        solana_sdk_ids::system_program,
        solana_signer::Signer,
        solana_svm::transaction_processor::ExecutionRecordingConfig,
        solana_system_interface::instruction::transfer,
        solana_transaction::Transaction,
        std::{
            num::NonZeroU64,
            sync::{
                atomic::{AtomicBool, Ordering},
                Arc, Mutex,
            },
            thread::sleep,
            time::Duration,
        },
        tempfile::TempDir,
    };

    fn find_free_addr() -> std::net::SocketAddr {
        crate::ephemeral_runtime::find_free_test_addr()
    }

    fn cluster_info() -> Arc<ClusterInfo> {
        let keypair = Arc::new(Keypair::new());
        Arc::new(ClusterInfo::new(
            ContactInfo::new_localhost(&keypair.pubkey(), 0),
            keypair,
            SocketAddrSpace::Unspecified,
        ))
    }

    #[test]
    fn abs_does_not_panic_while_er_shares_accounts_db() {
        setup();

        // Fund a sender on the L1 root bank, wrap in BankForks.
        let genesis_config = GenesisConfig::new(&[], &[]);
        let (root_bank, bank_forks) = Bank::new_with_bank_forks_for_tests(&genesis_config);
        let sender = Keypair::new();
        let initial_lamports = 100_000_000_000u64;
        root_bank.store_account(
            &sender.pubkey(),
            &solana_account::AccountSharedData::new(initial_lamports, 0, &system_program::id()),
        );
        root_bank.fill_bank_with_ticks_for_tests();
        root_bank.freeze();
        root_bank.set_block_id(Some(root_bank.hash()));

        // Wire ABS drop-callback plumbing against the L1 BankForks.
        root_bank
            .rc
            .accounts
            .accounts_db
            .enable_bank_drop_callback();
        let (pruned_banks_sender, pruned_banks_receiver) = unbounded();
        for bank in bank_forks.read().unwrap().banks().values() {
            bank.set_callback(Some(Box::new(SendDroppedBankCallback::new(
                pruned_banks_sender.clone(),
            ))));
        }

        // Start ER against the current L1 root.
        let l1_parent = bank_forks.read().unwrap().root_bank();
        let er_settings = EphemeralRollupSettings {
            session_pda: Pubkey::new_unique(),
            grid_id: 0,
            ttl_slots: 10_000,
            fee_cap: 1_000,
            er_fee_structure: EphemeralRollupSettings::zero_fee_structure(),
            delegated_accounts: vec![],
        };
        let mut er_runtime = EphemeralRuntime::new(
            l1_parent.clone(),
            cluster_info(),
            er_settings,
            find_free_addr(),
            find_free_addr(),
            find_free_addr(),
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
        )
        .unwrap();
        let initial_er_slot = er_runtime.bank().slot();

        // Commit a handful of transactions on the ER. Each commit writes to
        // the shared AccountsDb at the ER slot, populating its slot cache
        // and (on freeze/flush) its storage.
        for i in 0..8u64 {
            let er_bank = er_runtime.bank();
            let blockhash = er_bank.last_blockhash();
            let recipient = Pubkey::new_unique();
            let tx = Transaction::new_unsigned(Message::new_with_blockhash(
                &[transfer(&sender.pubkey(), &recipient, 1_000 + i)],
                Some(&sender.pubkey()),
                &blockhash,
            ));
            let batch = er_bank.prepare_batch_for_tests(vec![tx]);
            let mut timings = solana_svm_timings::ExecuteTimings::default();
            let _ = er_bank.load_execute_and_commit_transactions(
                &batch,
                ExecutionRecordingConfig::default(),
                &mut timings,
                None,
            );
        }

        // Spin up AccountsBackgroundService on the L1 BankForks.
        let (snapshot_request_sender, snapshot_request_receiver) = unbounded();
        let pending_snapshot_packages = Arc::new(Mutex::new(PendingSnapshotPackages::default()));
        let full_dir = TempDir::new().unwrap();
        let incr_dir = TempDir::new().unwrap();
        let bank_snap_dir = TempDir::new().unwrap();
        let snapshot_config = SnapshotConfig {
            full_snapshot_archive_interval: SnapshotInterval::Slots(NonZeroU64::new(4).unwrap()),
            incremental_snapshot_archive_interval: SnapshotInterval::Slots(
                NonZeroU64::new(2).unwrap(),
            ),
            full_snapshot_archives_dir: full_dir.path().to_path_buf(),
            incremental_snapshot_archives_dir: incr_dir.path().to_path_buf(),
            bank_snapshots_dir: bank_snap_dir.path().to_path_buf(),
            ..SnapshotConfig::default()
        };
        let snapshot_controller = Arc::new(SnapshotController::new(
            snapshot_request_sender,
            snapshot_config,
            bank_forks.read().unwrap().root(),
        ));
        let abs_handlers = AbsRequestHandlers {
            snapshot_request_handler: SnapshotRequestHandler {
                snapshot_controller: snapshot_controller.clone(),
                snapshot_request_receiver,
                pending_snapshot_packages,
            },
            pruned_banks_request_handler: PrunedBanksRequestHandler {
                pruned_banks_receiver,
            },
        };
        let exit = Arc::new(AtomicBool::new(false));
        let abs = AccountsBackgroundService::new(bank_forks.clone(), exit.clone(), abs_handlers);

        // Advance L1 through many slots and set roots regularly. On the old
        // `er_slot_for` implementation (ER placed inside the parent's epoch)
        // L1 would catch up to the ER's slot here and ABS would panic in
        // `purge_slot_cache_pubkeys`.
        const LAST_SLOT: u64 = 64;
        const SET_ROOT_EVERY: u64 = 4;
        for slot in 1..=LAST_SLOT {
            let parent = bank_forks.read().unwrap().get(slot - 1).unwrap();
            let child = Bank::new_from_parent(parent, SlotLeader::default(), slot);
            let child = bank_forks
                .write()
                .unwrap()
                .insert(child)
                .clone_without_scheduler();
            // A tiny tx per slot so the cache has real work to flush.
            let recipient = Pubkey::new_unique();
            let tx = Transaction::new_signed_with_payer(
                &[transfer(&sender.pubkey(), &recipient, 1)],
                Some(&sender.pubkey()),
                &[&sender],
                child.last_blockhash(),
            );
            let _ = child.process_transaction(&tx);
            child.fill_bank_with_ticks_for_tests();
            child.freeze();
            child.set_block_id(Some(child.hash()));

            if slot % SET_ROOT_EVERY == 0 {
                bank_forks
                    .write()
                    .unwrap()
                    .set_root(slot, Some(&snapshot_controller), None);
            }

            // Let ABS observe each batch.
            sleep(Duration::from_millis(20));
            assert!(
                abs.status().is_running(),
                "AccountsBackgroundService exited unexpectedly at slot {slot} — likely the \
                 shared-AccountsDb purge_slot_cache_pubkeys panic",
            );
        }

        // Shut everything down cleanly.
        exit.store(true, Ordering::Relaxed);
        abs.join()
            .expect("AccountsBackgroundService thread panicked");

        // Invariant: ER slots must be far enough ahead of any L1 slot the
        // validator will realistically reach, so the shared AccountsDb
        // never sees overlapping cache/storage entries. Require that the
        // furthest ER slot is at least 2^30 slots ahead of the furthest L1
        // slot we advanced to in this test.
        const MIN_SLOT_GAP: u64 = 1u64 << 30;
        let final_er_slot = er_runtime.bank().slot();
        assert!(
            initial_er_slot.saturating_sub(LAST_SLOT) >= MIN_SLOT_GAP,
            "initial ER slot {initial_er_slot} is too close to L1 last slot {LAST_SLOT}",
        );
        assert!(
            final_er_slot.saturating_sub(LAST_SLOT) >= MIN_SLOT_GAP,
            "final ER slot {final_er_slot} is too close to L1 last slot {LAST_SLOT}",
        );

        er_runtime.shutdown();
    }
}
