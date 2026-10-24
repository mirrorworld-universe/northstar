// Sonic: Opt-in authoritative inputs for a separately versioned VRF replay candidate.
use {
    super::Bank,
    serde::{Deserialize, Serialize},
    solana_account::{AccountSharedData, ReadableAccount},
    solana_loader_v3_interface::state::UpgradeableLoaderState,
    solana_pubkey::Pubkey,
    solana_sdk_ids::{bpf_loader_upgradeable, compute_budget, sysvar},
    solana_sha256_hasher::hashv,
    solana_transaction::versioned::VersionedTransaction,
};

pub const CANDIDATE_CAPTURE_VERSION: u16 = 1;
pub const CANDIDATE_CAPTURE_DOMAIN: &str = "northstar.er-vrf-candidate-capture.v1";
pub const MAX_CANDIDATE_ACCOUNT_BYTES: usize = 2 * 1024 * 1024;
pub const PROGRAMS: [(Pubkey, &str); 3] = [
    (
        Pubkey::from_str_const("FTDEMxTj2RsKBSCGETrFXDiSLu1nJLRfAYDsjJ6GXQ4N"),
        "441d8f463ecfcc398d2151f1bb4a1e221cff8b5fff1d9f185db90de6f78b8cde",
    ),
    (
        Pubkey::from_str_const("FUBz1nvSSoHnMqFoK2CA3jF5ckpYPdF3ZT4SoLzDNzh1"),
        "225fc076dc79661768bf6a48bbac0d26596d00ad15a8bddcf99dc4a76ad4aa88",
    ),
    (
        Pubkey::from_str_const("HFg1j9ZMixGkMo42ZoeqNpqBZpGcVbsRwfFBLcGCn5Kt"),
        "b7662ad1e4ebca17f1a8f2e222f18fcb1dcacf1e8daaad7d709f67f92798c4a1",
    ),
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateAccount {
    pub lamports: u64,
    pub data: Vec<u8>,
    pub owner: String,
    pub executable: bool,
    pub rent_epoch: u64,
}
impl CandidateAccount {
    pub fn from_shared(account: &AccountSharedData) -> Self {
        Self {
            lamports: account.lamports(),
            data: account.data().to_vec(),
            owner: account.owner().to_string(),
            executable: account.executable(),
            rent_epoch: account.rent_epoch(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateAccountEntry {
    pub pubkey: String,
    pub account: Option<CandidateAccount>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateProgram {
    pub program_id: String,
    pub programdata_address: String,
    pub deployment_slot: u64,
    pub elf_sha256: String,
    pub elf_offset: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateSysvar {
    pub pubkey: String,
    pub cached_data: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErVrfCandidateSnapshot {
    pub version: u16,
    pub domain: String,
    pub profile: String,
    pub slot: u64,
    pub epoch: u64,
    pub signed_transaction: Vec<u8>,
    pub message_account_keys: Vec<String>,
    pub accounts: Vec<CandidateAccountEntry>,
    pub programs: Vec<CandidateProgram>,
    pub sysvars: Vec<CandidateSysvar>,
    pub active_features: Vec<(String, u64)>,
    pub inactive_features: Vec<String>,
    pub reserved_account_keys: Vec<String>,
    pub blockhash_queue_bincode: Vec<u8>,
    pub fee_rate_governor_bincode: Vec<u8>,
    pub fee_structure: CandidateFeePolicy,
    pub recent_blockhashes: Vec<CandidateRecentBlockhash>,
    pub latest_blockhash: String,
    pub blockhash_lamports_per_signature: u64,
    pub transaction_hash_age: u64,
    pub max_processing_age: usize,
    pub transaction_max_age: usize,
    pub epoch_total_stake: u64,
    pub compute_unit_limit: u64,
    pub compute_budget_override: bool,
    pub lamports_per_signature: u64,
    pub zero_fee_policy: bool,
    pub vm_config_debug: String,
    pub syscall_registry: Vec<(u32, Vec<u8>)>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateFeePolicy {
    pub lamports_per_signature: u64,
    pub lamports_per_write_lock: u64,
    pub compute_fee_bins: Vec<(u64, u64)>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateRecentBlockhash {
    pub blockhash: String,
    pub age: u64,
    pub lamports_per_signature: u64,
}

pub fn digest(bytes: &[u8]) -> String {
    hashv(&[bytes])
        .to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl Bank {
    pub fn er_vrf_candidate_snapshot(
        &self,
        tx: &VersionedTransaction,
        transaction_max_age: usize,
    ) -> Result<ErVrfCandidateSnapshot, &'static str> {
        let solana_message::VersionedMessage::Legacy(message) = &tx.message else {
            return Err("legacy transactions only");
        };
        if message.account_keys.len() > 32
            || tx.signatures.len() != 1
            || message.header.num_required_signatures != 1
            || message.instructions.len() != 2
        {
            return Err("unsupported message shape");
        }
        let budget = &message.instructions[0];
        if message.account_keys.get(budget.program_id_index as usize) != Some(&compute_budget::id())
            || !budget.accounts.is_empty()
            || budget.data.len() != 5
            || budget.data[0] != 2
        {
            return Err("one explicit compute unit limit required");
        }
        let limit = u32::from_le_bytes(budget.data[1..5].try_into().unwrap());
        if limit == 0 || limit > 1_400_000 {
            return Err("compute unit limit outside bound");
        }
        let instruction = &message.instructions[1];
        let target = message
            .account_keys
            .get(instruction.program_id_index as usize)
            .ok_or("program index out of range")?;
        let profile = if target == &PROGRAMS[2].0
            && instruction.data.get(..8) == Some(&hashv(&[b"global:make_request"]).to_bytes()[..8])
        {
            "consumer.make_request"
        } else if target == &PROGRAMS[0].0
            && instruction.data.get(..8) == Some(&hashv(&[b"global:fulfill"]).to_bytes()[..8])
        {
            "coordinator.fulfill"
        } else {
            return Err("unsupported pinned composition instruction");
        };
        if self.fee_structure.lamports_per_signature != 0
            || self.fee_structure.lamports_per_write_lock != 0
            || !self.fee_structure.compute_fee_bins.is_empty()
            || self.compute_budget.is_some()
        {
            return Err("candidate requires explicit zero fee and default runtime budget");
        }
        let hash_age = self
            .get_hash_age(&message.recent_blockhash)
            .ok_or("recent blockhash absent")?;
        if hash_age > transaction_max_age as u64 {
            return Err("expired blockhash");
        }
        let mut keys = message.account_keys.clone();
        let mut programs = Vec::new();
        for (program_id, expected_digest) in PROGRAMS {
            let program = self.get_account(&program_id).ok_or("program absent")?;
            if program.owner() != &bpf_loader_upgradeable::id() || !program.executable() {
                return Err("program loader binding");
            }
            let UpgradeableLoaderState::Program {
                programdata_address,
            } = bincode::deserialize(program.data()).map_err(|_| "program header")?
            else {
                return Err("program header variant");
            };
            if programdata_address
                != Pubkey::find_program_address(
                    &[program_id.as_ref()],
                    &bpf_loader_upgradeable::id(),
                )
                .0
            {
                return Err("programdata PDA binding");
            }
            let data = self
                .get_account(&programdata_address)
                .ok_or("programdata absent")?;
            if data.owner() != &bpf_loader_upgradeable::id() || data.executable() {
                return Err("programdata owner binding");
            }
            let UpgradeableLoaderState::ProgramData { slot, .. } =
                bincode::deserialize(data.data()).map_err(|_| "programdata header")?
            else {
                return Err("programdata header variant");
            };
            if slot >= self.slot() {
                return Err("programdata not yet visible");
            }
            let offset = UpgradeableLoaderState::size_of_programdata_metadata();
            let elf = data.data().get(offset..).ok_or("programdata ELF range")?;
            let elf_sha256 = digest(elf);
            if elf_sha256 != expected_digest {
                return Err("pinned ELF mismatch");
            }
            programs.push(CandidateProgram {
                program_id: program_id.to_string(),
                programdata_address: programdata_address.to_string(),
                deployment_slot: slot,
                elf_sha256,
                elf_offset: offset,
            });
            keys.extend([program_id, programdata_address]);
        }
        let sysvar_ids = [
            sysvar::clock::id(),
            sysvar::rent::id(),
            sysvar::epoch_schedule::id(),
            sysvar::slot_hashes::id(),
            sysvar::stake_history::id(),
            sysvar::epoch_rewards::id(),
            sysvar::last_restart_slot::id(),
        ];
        let cache = self.transaction_processor.sysvar_cache();
        let mut sysvars = Vec::new();
        for (index, key) in sysvar_ids.into_iter().enumerate() {
            let account = self.get_account(&key);
            let cached_data = cache.sysvar_id_to_buffer(&key).clone();
            if index < 4 && (account.is_none() || cached_data.is_none()) {
                return Err("required sysvar absent");
            }
            if account.as_ref().map(|value| value.data()) != cached_data.as_deref() {
                return Err("sysvar cache/account mismatch");
            }
            sysvars.push(CandidateSysvar {
                pubkey: key.to_string(),
                cached_data,
            });
            keys.push(key);
        }
        keys.extend([bpf_loader_upgradeable::id(), sysvar::instructions::id()]);
        keys.sort_unstable();
        keys.dedup();
        let mut size = 0usize;
        let mut accounts = Vec::new();
        for key in keys {
            let account = self.get_account(&key);
            if account.as_ref().is_some_and(|account| account.executable())
                && !PROGRAMS.iter().any(|(program, _)| program == &key)
                && key != solana_sdk_ids::system_program::id()
                && key != compute_budget::id()
                && key != bpf_loader_upgradeable::id()
            {
                return Err("unreviewed executable account");
            }
            size = size
                .checked_add(account.as_ref().map_or(0, |value| value.data().len()))
                .ok_or("account size overflow")?;
            if size > MAX_CANDIDATE_ACCOUNT_BYTES {
                return Err("account byte bound");
            }
            accounts.push(CandidateAccountEntry {
                pubkey: key.to_string(),
                account: account.as_ref().map(CandidateAccount::from_shared),
            });
        }
        let mut active_features: Vec<_> = self
            .feature_set
            .active()
            .iter()
            .map(|(key, slot)| (key.to_string(), *slot))
            .collect();
        active_features.sort_unstable();
        let mut inactive_features: Vec<_> = self
            .feature_set
            .inactive()
            .iter()
            .map(ToString::to_string)
            .collect();
        inactive_features.sort_unstable();
        let mut reserved_account_keys: Vec<_> = self
            .get_reserved_account_keys()
            .iter()
            .map(ToString::to_string)
            .collect();
        reserved_account_keys.sort_unstable();
        let (blockhash, blockhash_fee) = self.last_blockhash_and_lamports_per_signature();
        Ok(ErVrfCandidateSnapshot {
            version: CANDIDATE_CAPTURE_VERSION,
            domain: CANDIDATE_CAPTURE_DOMAIN.into(),
            profile: profile.into(),
            slot: self.slot(),
            epoch: self.epoch(),
            signed_transaction: bincode::serialize(tx).map_err(|_| "transaction serialization")?,
            message_account_keys: message
                .account_keys
                .iter()
                .map(ToString::to_string)
                .collect(),
            accounts,
            programs,
            sysvars,
            active_features,
            inactive_features,
            reserved_account_keys,
            blockhash_queue_bincode: bincode::serialize(&*self.blockhash_queue.read().unwrap())
                .map_err(|_| "blockhash queue serialization")?,
            fee_rate_governor_bincode: bincode::serialize(&self.fee_rate_governor)
                .map_err(|_| "fee governor serialization")?,
            fee_structure: CandidateFeePolicy {
                lamports_per_signature: 0,
                lamports_per_write_lock: 0,
                compute_fee_bins: vec![],
            },
            recent_blockhashes: {
                let queue = self.blockhash_queue.read().unwrap();
                let mut entries = queue
                    .capture_entries()
                    .map(|(hash, age, fee)| {
                        Ok(CandidateRecentBlockhash {
                            blockhash: hash.to_string(),
                            age: age.ok_or("blockhash age inversion")?,
                            lamports_per_signature: fee,
                        })
                    })
                    .collect::<Result<Vec<_>, &'static str>>()?;
                if entries.len() > 4096 {
                    return Err("blockhash queue bound");
                }
                entries.sort_unstable_by(|a, b| a.blockhash.cmp(&b.blockhash));
                entries
            },
            latest_blockhash: blockhash.to_string(),
            blockhash_lamports_per_signature: blockhash_fee,
            transaction_hash_age: hash_age,
            max_processing_age: self.max_processing_age(),
            transaction_max_age,
            epoch_total_stake: self.get_current_epoch_total_stake(),
            compute_unit_limit: u64::from(limit),
            compute_budget_override: false,
            lamports_per_signature: 0,
            zero_fee_policy: true,
            vm_config_debug: format!(
                "{:?}",
                self.transaction_processor
                    .program_runtime_environment
                    .get_config()
            ),
            syscall_registry: self
                .transaction_processor
                .program_runtime_environment
                .get_function_registry()
                .iter()
                .map(|(key, (name, _))| (key, name.to_vec()))
                .collect(),
        })
    }
}
