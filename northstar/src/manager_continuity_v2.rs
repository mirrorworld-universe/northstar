use {
    super::*,
    crate::{continuity_v2::hash_parts, lineage_store_v2::StartupModeV2},
    std::{fs, io, os::unix::fs::DirBuilderExt, path::Path},
};

pub(super) type PreparedContinuitySettlement = (u64, [u8; 32], Vec<Transaction>);

fn create_lineage_parents(directory: &Path) -> io::Result<()> {
    let mut parent = directory.parent().unwrap_or_else(|| Path::new("."));
    let mut missing = Vec::new();
    loop {
        if parent.as_os_str().is_empty() {
            parent = Path::new(".");
        }
        match fs::symlink_metadata(parent) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => break,
            Ok(_) => {
                return Err(io::Error::other(
                    "lineage parent is not a regular directory",
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(parent.to_path_buf());
                parent = parent.parent().unwrap_or_else(|| Path::new("."));
            }
            Err(error) => return Err(error),
        }
    }
    for path in missing.into_iter().rev() {
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        fs::File::open(
            path.parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
        )?
        .sync_all()?;
    }
    Ok(())
}

impl Manager {
    pub(super) fn continuity_settlement_transactions_if_due(
        &self,
        bank: &Bank,
        blockhash: Hash,
    ) -> Option<PreparedContinuitySettlement> {
        match self.build_continuity_settlement(bank, blockhash) {
            Ok(value) => value,
            Err(error) => {
                if let Some(runtime) = &self.runtime {
                    runtime.quarantine_checkpoint("checkpoint_lineage_recovery_failed");
                }
                warn!("Continuity settlement quarantined before signing: {error}");
                None
            }
        }
    }

    fn build_continuity_settlement(
        &self,
        bank: &Bank,
        blockhash: Hash,
    ) -> io::Result<Option<PreparedContinuitySettlement>> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| io::Error::other("runtime missing"))?;
        if !runtime.is_active() {
            return Ok(None);
        }
        #[cfg(feature = "proof-coordinator")]
        if self.proof_coordinator.is_some() {
            return Err(io::Error::other(
                "continuity proof coordination requires a separately qualified profile",
            ));
        }
        let (session, frame) =
            crate::lineage_l1_v2::session_from_l1_v2(bank, &self.config.portal_program_id)?
                .ok_or_else(|| io::Error::other("continuity session missing"))?;
        let store = runtime
            .lineage_store_v2()
            .ok_or_else(|| io::Error::other("lineage store missing"))?;
        {
            let store = store.lock().unwrap();
            if store.is_poisoned() {
                return Err(io::Error::other(
                    "lineage persistence requires restart reconciliation",
                ));
            }
            if frame.identity(&self.config.portal_program_id, &session)
                != *store.accepted_state().identity()
            {
                return Err(io::Error::other("continuity settlement identity differs"));
            }
            if store.sealed().is_none()
                && bank.slot()
                    < frame
                        .base
                        .last_settled_l1_slot
                        .saturating_add(frame.base.settlement_interval_slots)
                && store.unsealed().len() < crate::checkpoint_v2::MAX_CHECKPOINT_TRANSITIONS_V2
            {
                return Ok(None);
            }
        }
        if !runtime.seal_checkpoint_if_nonempty() {
            return Ok(None);
        }
        let artifact = runtime
            .checkpoint_artifact_v2()
            .ok_or_else(|| io::Error::other("sealed continuity artifact missing"))?;
        artifact.validate().map_err(io::Error::other)?;
        let header = artifact.checkpoint;
        let (checkpoint, _) =
            find_checkpoint_pda(&self.config.portal_program_id, &session, header.er_slot);
        let checkpoint_account = bank.get_account(&checkpoint);
        let saved_plan = store.lock().unwrap().settlement_plan()?;
        let plan = if let Some(plan) = saved_plan {
            plan
        } else {
            if checkpoint_account.is_some() {
                return Err(io::Error::other(
                    "on-chain checkpoint has no durable settlement plan",
                ));
            }
            let plan = self
                .settlement_plan()
                .ok_or_else(|| io::Error::other("continuity settlement plan missing"))?;
            store.lock().unwrap().bind_settlement_plan(&plan)?;
            plan
        };
        let validator = self.config.manager_account.as_ref();
        let transaction = |instruction| {
            Transaction::new_signed_with_payer(
                &[instruction],
                Some(&validator.pubkey()),
                &[validator],
                blockhash,
            )
        };
        let (cursor, cursor_bump) =
            find_checkpoint_cursor_pda(&self.config.portal_program_id, &session);
        if let Some(account) = bank.get_account(&cursor) {
            let value: northstar_portal::CheckpointCursor = borsh::from_slice(account.data())?;
            if account.owner() != &self.config.portal_program_id
                || account.executable()
                || !value.is_valid()
                || value.session != session
                || value.bump != cursor_bump
                || (value.active_checkpoint != Pubkey::default()
                    && (checkpoint_account.is_none() || value.active_checkpoint != checkpoint))
            {
                return Err(io::Error::other("continuity checkpoint cursor differs"));
            }
        } else if checkpoint_account.is_some() {
            return Err(io::Error::other("established continuity cursor missing"));
        }
        let (binding, binding_bump) = northstar_portal::find_checkpoint_binding_v2_pda(
            &self.config.portal_program_id,
            &checkpoint,
        );
        let transactions = if let Some(account) = checkpoint_account {
            if account.owner() != &self.config.portal_program_id || account.executable() {
                return Err(io::Error::other("continuity checkpoint owner differs"));
            }
            let accepted: Checkpoint = borsh::from_slice(account.data())?;
            let binding_account = bank
                .get_account(&binding)
                .ok_or_else(|| io::Error::other("checkpoint binding missing"))?;
            let bound: northstar_portal::CheckpointBindingV2 =
                borsh::from_slice(binding_account.data())?;
            if binding_account.owner() != &self.config.portal_program_id
                || binding_account.executable()
                || bound.discriminator != northstar_portal::CheckpointBindingV2::DISCRIMINATOR
                || bound.bump != binding_bump
                || bound.checkpoint != checkpoint.to_bytes()
                || bound.pre != artifact.pre
                || bound.post != artifact.post
                || bound.input_context_root != header.readonly_l1_root
                || !accepted.is_valid()
                || accepted.session != session
                || accepted.er_slot != header.er_slot
                || accepted.proposer != validator.pubkey()
                || accepted.step_count != u64::from(header.step_count)
                || accepted.previous_state_root != header.previous_state_root
                || accepted.new_state_root != header.new_state_root
                || accepted.trace_root != header.trace_root
                || accepted.tx_effect_root != header.transaction_effect_root
                || accepted.readonly_l1_root != header.readonly_l1_root
                || accepted.da_commitment != header.da_commitment
                || accepted.effect_commitment != header.effect_commitment
            {
                return Err(io::Error::other(
                    "on-chain checkpoint differs from the retained artifact",
                ));
            }
            match accepted.status {
                CheckpointStatus::Pending => {
                    if bank.slot() <= accepted.challenge_deadline_l1_slot {
                        return Ok(None);
                    }
                    vec![transaction(Instruction {
                        program_id: self.config.portal_program_id,
                        accounts: vec![
                            AccountMeta::new(validator.pubkey(), true),
                            AccountMeta::new(session, false),
                            AccountMeta::new(checkpoint, false),
                            AccountMeta::new(cursor, false),
                            AccountMeta::new(accepted.proposer, false),
                            AccountMeta::new_readonly(binding, false),
                        ],
                        data: borsh::to_vec(
                            &northstar_portal::PortalInstruction::CommitCheckpointV2(
                                northstar_portal::CommitCheckpoint {
                                    er_slot: header.er_slot,
                                },
                            ),
                        )?,
                    })]
                }
                CheckpointStatus::Committed => {
                    let accumulator =
                        if frame.base.settlement_status == SettlementStatus::InProgress {
                            if frame.base.settlement_er_slot != plan.er_slot
                                || frame.base.settlement_checksum != plan.checksum
                            {
                                return Err(io::Error::other(
                                    "active settlement differs from its durable plan",
                                ));
                            }
                            Some(frame.base.settlement_accumulator)
                        } else {
                            None
                        };
                    plan.portal_transactions_with_effect_commitment(
                        self.config.portal_program_id,
                        session,
                        validator,
                        blockhash,
                        header.effect_commitment,
                        accumulator,
                    )
                }
                CheckpointStatus::Settled => return Ok(None),
                _ => {
                    return Err(io::Error::other(
                        "continuity checkpoint requires dispute or cancellation recovery",
                    ))
                }
            }
        } else {
            let prior = crate::lineage_l1_v2::checkpoint_head_from_l1_v2(
                bank,
                &self.config.portal_program_id,
                &session,
                &frame,
            )?;
            if let Some((head, _)) = prior {
                if head.state_root != header.previous_state_root || head.er_slot >= header.er_slot {
                    return Err(io::Error::other(
                        "continuity checkpoint previous root differs",
                    ));
                }
            } else if artifact.pre
                != northstar_zk_types::continuity::empty_state_opening_v2(
                    artifact.pre.session_context,
                    hash_parts,
                )
                .map_err(io::Error::other)?
            {
                return Err(io::Error::other(
                    "first continuity checkpoint does not open the canonical empty state",
                ));
            }
            let prefix = if artifact.post.ingress_count == 0 {
                system_program::id()
            } else {
                northstar_portal::find_ingress_record_v2_pda(
                    &self.config.portal_program_id,
                    &artifact.post.session_context,
                    artifact.post.ingress_count,
                )
                .0
            };
            let proposal = northstar_portal::ProposeCheckpoint {
                er_slot: header.er_slot,
                step_count: u64::from(header.step_count),
                previous_state_root: header.previous_state_root,
                new_state_root: header.new_state_root,
                trace_root: header.trace_root,
                tx_effect_root: header.transaction_effect_root,
                readonly_l1_root: header.readonly_l1_root,
                da_commitment: header.da_commitment,
                effect_commitment: header.effect_commitment,
                challenge_window_slots: DEFAULT_CHECKPOINT_CHALLENGE_WINDOW_SLOTS,
            };
            vec![transaction(Instruction {
                program_id: self.config.portal_program_id,
                accounts: vec![
                    AccountMeta::new(validator.pubkey(), true),
                    AccountMeta::new_readonly(session, false),
                    AccountMeta::new(checkpoint, false),
                    AccountMeta::new(cursor, false),
                    AccountMeta::new_readonly(system_program::id(), false),
                    AccountMeta::new(binding, false),
                    AccountMeta::new_readonly(prefix, false),
                ],
                data: borsh::to_vec(&northstar_portal::PortalInstruction::ProposeCheckpointV2(
                    northstar_portal::ProposeCheckpointV2 {
                        proposal,
                        pre: artifact.pre,
                        post: artifact.post,
                        readonly_l1_root: artifact.readonly_l1_root,
                    },
                ))?,
            })]
        };
        Ok((!transactions.is_empty()).then_some((plan.er_slot, plan.checksum, transactions)))
    }

    pub(super) fn prepare_continuity_session(
        &mut self,
        bank: Arc<Bank>,
        session: Pubkey,
        mode: StartupModeV2,
    ) -> io::Result<()> {
        let (actual, frame) =
            crate::lineage_l1_v2::session_from_l1_v2(&bank, &self.config.portal_program_id)?
                .ok_or_else(|| io::Error::other("continuity session is missing"))?;
        if actual != session
            || frame.base.validator != self.config.manager_account.pubkey()
            || frame.base.is_expired(bank.slot())
        {
            return Err(io::Error::other(
                "continuity session identity differs or expired",
            ));
        }
        let identity = frame.identity(&self.config.portal_program_id, &session);
        let context = identity.commitment(hash_parts).map_err(io::Error::other)?;
        let directory = self
            .config
            .checkpoint_plan_dir
            .clone()
            .or_else(|| std::env::var_os("NORTHSTAR_CHECKPOINT_PLAN_DIR").map(PathBuf::from))
            .ok_or_else(|| io::Error::other("continuity requires an explicit durable directory"))?
            .join("lineage-v2")
            .join(Hash::new_from_array(context).to_string());
        if mode == StartupModeV2::InitializeNew {
            create_lineage_parents(&directory)?;
        }
        let runtime = self
            .runtime
            .as_mut()
            .ok_or_else(|| io::Error::other("continuity runtime is not initialized"))?;
        if let Some(store) = runtime.lineage_store_v2() {
            if store.lock().unwrap().accepted_state().identity() != &identity
                || !runtime.is_active()
            {
                return Err(io::Error::other(
                    "established continuity runtime requires isolated recovery",
                ));
            }
            return Ok(());
        }
        if runtime.is_active() {
            return Err(io::Error::other(
                "cannot replace an active runtime with another protocol",
            ));
        }
        runtime.set_session_settings(frame.base.grid_id, frame.base.ttl_slots, frame.base.fee_cap);
        runtime.reset_to_new_parent_with_slot_floor(bank, frame.base.last_settled_er_slot);
        runtime.set_session_pda(session);
        runtime.configure_checkpoint_v2(&directory, identity, mode)?;
        runtime.set_checkpoint_error(None);
        runtime.activate();
        Ok(())
    }

    pub(super) fn process_finalized_continuity_inputs(&self, bank: &Bank) {
        let Some(runtime) = self.runtime.as_ref().filter(|runtime| runtime.is_active()) else {
            return;
        };
        let Some(store) = runtime.lineage_store_v2() else {
            return;
        };
        let Some(session) = *runtime.session_pda().read().unwrap() else {
            return;
        };
        if bank.get_account(&session).is_none() && store.lock().unwrap().accepted_head().is_none() {
            return;
        }
        let result = runtime.reconcile_checkpoint_v2(bank).and_then(|_| {
            runtime.process_l1_ingress_v2(bank).and_then(|token| {
                if token.is_some() {
                    Err(io::Error::other(
                        "authenticated token ingress requires its execution adapter",
                    ))
                } else {
                    Ok(())
                }
            })
        });
        if let Err(error) = result {
            runtime.quarantine_checkpoint("checkpoint_lineage_recovery_failed");
            warn!("Continuity input processing quarantined before publication: {error}");
        }
    }
}
