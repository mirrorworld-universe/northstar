//! Bootstrap a shadow replay from a replica's own rooted L1 view. The reviewed
//! pins and the L1 node must be independent of the ER proposing transactions.
//! This module has no vote, proof, submission, or validator activation API.
use {
    crate::{
        checkpoint_v2::MAX_CHECKPOINT_TRANSITIONS_V2,
        continuity_v2::{hash_parts, CommittedStateV2, Hash32},
        lineage_l1_v2::{checkpoint_head_from_l1_v2, session_from_l1_v2},
        lineage_store_v2::L1CheckpointHeadV2,
    },
    borsh::BorshDeserialize,
    northstar_portal::{CheckpointBindingV2, CheckpointBondStatus, CheckpointStatus},
    northstar_zk_types::continuity::SessionIdentityV2,
    solana_account::ReadableAccount,
    solana_genesis_config::GenesisConfig,
    solana_pubkey::Pubkey,
    solana_runtime::{bank::Bank, bank_forks::BankForks},
    solana_sdk_ids::bpf_loader_upgradeable,
    std::io,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgramImage {
    pub address: Pubkey,
    /// SHA-256 of the entire finalized loader ProgramData account, including metadata.
    pub programdata_sha256: Hash32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootstrapPins {
    pub identity: SessionIdentityV2,
    pub checkpoint: L1CheckpointHeadV2,
    pub finalized_l1_slot_floor: u64,
    pub minimum_challenge_slots: u64,
    pub portal: ProgramImage,
    pub coordinator: ProgramImage,
    pub verifier: ProgramImage,
    pub consumer: ProgramImage,
}

/// Distinct from the live lineage store: obtaining a matching opening here does
/// not move an accepted head, certify an ER child, or authorize a VRF proof.
pub struct ReplayBootstrap {
    pins: BootstrapPins,
    observed_l1_slot: u64,
    state: CommittedStateV2,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn verify_image(bank: &Bank, image: ProgramImage) -> io::Result<()> {
    let loader = bpf_loader_upgradeable::id();
    let data_key = Pubkey::find_program_address(&[image.address.as_ref()], &loader).0;
    let program = bank
        .get_account(&image.address)
        .ok_or_else(|| invalid("missing program"))?;
    let data = bank
        .get_account(&data_key)
        .ok_or_else(|| invalid("missing ProgramData"))?;
    if !program.executable()
        || program.owner() != &loader
        || program.data().len() != 36
        || program.data()[..4] != 2u32.to_le_bytes()
        || program.data()[4..] != data_key.to_bytes()
        || data.executable()
        || data.owner() != &loader
        || data.data().len() <= 45
        || data.data()[..4] != 3u32.to_le_bytes()
        || hash_parts(&[data.data()]) != image.programdata_sha256
    {
        return Err(invalid("rooted program image differs"));
    }
    Ok(())
}

impl ReplayBootstrap {
    /// Use the GenesisConfig and BankForks of the replica's independently synced
    /// L1 node, not a leader-provided bank, genesis string, or working tip. The
    /// candidate state may come from untrusted storage; only its canonical opening
    /// is compared. Authentication of external epoch enrollment is a separate gate.
    pub fn from_root(
        genesis: &GenesisConfig,
        l1: &BankForks,
        pins: BootstrapPins,
        candidate: CommittedStateV2,
    ) -> io::Result<Self> {
        let root = l1.root_bank();
        let slot = root.slot();
        let images = [pins.portal, pins.coordinator, pins.verifier, pins.consumer];
        if !root.is_frozen()
            || slot < pins.finalized_l1_slot_floor
            || pins.finalized_l1_slot_floor == 0
            || genesis.hash().to_bytes() != pins.identity.l1_genesis
            || candidate.identity() != &pins.identity
            || pins.portal.address.to_bytes() != pins.identity.portal
            || !(10..=72_000).contains(&pins.minimum_challenge_slots)
            || images.iter().enumerate().any(|(i, image)| {
                image.address == Pubkey::default()
                    || image.programdata_sha256 == [0; 32]
                    || images[..i]
                        .iter()
                        .any(|prior| prior.address == image.address)
            })
        {
            return Err(invalid("invalid replay bootstrap pins or L1 root"));
        }
        let portal = pins.portal.address;
        let (session, frame) =
            session_from_l1_v2(&root, &portal)?.ok_or_else(|| invalid("missing rooted session"))?;
        if frame.identity(&portal, &session) != pins.identity
            || slot < frame.base.created_at
            || !frame
                .base
                .created_at
                .checked_add(frame.base.ttl_slots)
                .is_some_and(|last| slot <= last)
        {
            return Err(invalid("rooted session differs or is not live"));
        }
        let (head, checkpoint) = checkpoint_head_from_l1_v2(&root, &portal, &session, &frame)?
            .ok_or_else(|| invalid("missing rooted checkpoint"))?;
        let checkpoint_key = Pubkey::from(head.checkpoint);
        let (key, bump) = northstar_portal::find_checkpoint_pda(&portal, &session, head.er_slot);
        if head != pins.checkpoint
            || head.er_slot == 0
            || key != checkpoint_key
            || bump != checkpoint.bump
            || checkpoint.status != CheckpointStatus::Settled
            || checkpoint.bond_status != CheckpointBondStatus::Released
            || !(1..=MAX_CHECKPOINT_TRANSITIONS_V2 as u64).contains(&checkpoint.step_count)
            || frame.base.last_settled_er_slot != head.er_slot
            || frame.base.last_settled_l1_slot > slot
            || checkpoint.challenge_deadline_l1_slot > frame.base.last_settled_l1_slot
            || !checkpoint
                .challenge_deadline_l1_slot
                .checked_sub(checkpoint.proposed_at_l1_slot)
                .is_some_and(|n| n >= pins.minimum_challenge_slots)
        {
            return Err(invalid(
                "rooted checkpoint is not the pinned settled prefix",
            ));
        }
        let opening = candidate.opening().map_err(io::Error::other)?;
        let prefix = if opening.ingress_count == 0 {
            Pubkey::default()
        } else {
            northstar_portal::find_ingress_record_v2_pda(
                &portal,
                &opening.session_context,
                opening.ingress_count,
            )
            .0
        };
        // checkpoint_head_from_l1_v2 checked this PDA, its owner and its binding
        // against the cursor in the very same immutable root.
        let binding_key =
            northstar_portal::find_checkpoint_binding_v2_pda(&portal, &checkpoint_key).0;
        let binding = root
            .get_account(&binding_key)
            .ok_or_else(|| invalid("missing rooted binding"))?;
        let binding = CheckpointBindingV2::try_from_slice(binding.data())?;
        if opening != binding.post
            || binding.prefix_record != prefix.to_bytes()
            || opening.commitment(hash_parts).map_err(io::Error::other)? != head.state_root
        {
            return Err(invalid("candidate state differs from rooted opening"));
        }
        for image in images {
            verify_image(&root, image)?;
        }
        Ok(Self {
            pins,
            observed_l1_slot: slot,
            state: candidate,
        })
    }

    pub fn pins(&self) -> &BootstrapPins {
        &self.pins
    }
    pub fn observed_l1_slot(&self) -> u64 {
        self.observed_l1_slot
    }
    pub fn state(&self) -> &CommittedStateV2 {
        &self.state
    }
}

#[cfg(test)]
mod tests;
