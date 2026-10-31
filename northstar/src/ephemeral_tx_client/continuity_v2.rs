use {
    super::*,
    crate::continuity_v2::{
        CommittedAccountV2, CommittedStateV2, RecordedExecutionResultV2,
        RecordedInnerInstructionV2, RecordedTransitionV2, SettlementEffectV2, TransitionPayloadV2,
    },
    solana_svm::transaction_processing_result::{
        ProcessedTransaction, TransactionProcessingResult,
    },
};

impl EphemeralTransactionClient {
    pub(super) fn record_v2_before_commit(
        &self,
        bank: &Bank,
        transactions: &[VersionedTransaction],
        results: &[TransactionProcessingResult],
        replay: &[Option<Vec<u8>>],
        ingress: Option<northstar_zk_types::continuity::IngressRecordV2>,
    ) -> Result<(), solana_transaction::TransactionError> {
        let Some(store) = self.lineage_store_v2() else {
            return Ok(());
        };
        let mut store = store.lock().unwrap();
        let mut state = store
            .working_state()
            .map_err(|error| self.v2_persistence_failure(error))?;
        let mut records = Vec::new();
        if ingress.is_some() && transactions.len() != 1 {
            return Err(solana_transaction::TransactionError::CommitCancelled);
        }
        for (position, (transaction, result)) in transactions.iter().zip(results).enumerate() {
            let Ok(processed) = result else {
                continue;
            };
            if processed.fee_details().total_fee() != 0 {
                warn!("Nonzero ER fees are not supported by the continuity v2 development profile");
                return Err(solana_transaction::TransactionError::CommitCancelled);
            }
            let status = processed.status();
            if ingress.is_some() && status.is_err() {
                return Err(solana_transaction::TransactionError::CommitCancelled);
            }
            let mut outputs = BTreeMap::new();
            match processed {
                ProcessedTransaction::Executed(executed) if status.is_ok() => {
                    for ((key, value), touched) in executed
                        .loaded_transaction
                        .accounts
                        .iter()
                        .zip(&executed.loaded_transaction.touched_flags)
                    {
                        if *touched && !Self::is_infrastructure_account(key) {
                            outputs.insert(*key, value.clone());
                        }
                    }
                }
                ProcessedTransaction::Executed(executed) => {
                    outputs.extend(
                        executed
                            .loaded_transaction
                            .rollback_accounts
                            .iter()
                            .cloned(),
                    );
                }
                ProcessedTransaction::FeesOnly(details) => {
                    outputs.extend(details.rollback_accounts.iter().cloned())
                }
                ProcessedTransaction::NoOp(_) => {}
            }
            let pre = outputs
                .keys()
                .map(|key| {
                    (
                        key.to_bytes(),
                        CommittedAccountV2::from_account(
                            &bank.get_account(key).unwrap_or_default(),
                        ),
                    )
                })
                .collect::<Vec<_>>();
            let post = outputs
                .iter()
                .map(|(key, value)| (key.to_bytes(), CommittedAccountV2::from_account(value)))
                .collect::<Vec<_>>();
            let before = state
                .opening()
                .map_err(|error| self.v2_persistence_failure(error))?;
            let details = processed.execution_details();
            let mut inner = Vec::new();
            if let Some(instructions) =
                details.and_then(|details| details.inner_instructions.as_ref())
            {
                for (outer, instructions) in instructions.iter().enumerate() {
                    for instruction in instructions {
                        inner.push(RecordedInnerInstructionV2 {
                            outer_index: u32::try_from(outer).map_err(|_| {
                                solana_transaction::TransactionError::CommitCancelled
                            })?,
                            stack_height: u32::from(instruction.stack_height),
                            program_id_index: instruction.instruction.program_id_index,
                            accounts: instruction.instruction.accounts.clone(),
                            data: instruction.instruction.data.clone(),
                        });
                    }
                }
            }
            let output = RecordedExecutionResultV2 {
                status: bincode::serialize(&status)
                    .map_err(|_| solana_transaction::TransactionError::CommitCancelled)?,
                executed_units: processed.executed_units(),
                inner,
                logs: details
                    .and_then(|details| details.log_messages.clone())
                    .unwrap_or_default(),
                return_data: details
                    .and_then(|details| details.return_data.as_ref())
                    .map(|data| (data.program_id.to_bytes(), data.data.clone())),
            };
            let execution_result = borsh::to_vec(&output)
                .map_err(|_| solana_transaction::TransactionError::CommitCancelled)?;
            let wire = bincode::serialize(transaction)
                .map_err(|_| solana_transaction::TransactionError::CommitCancelled)?;
            let replay = replay
                .get(position)
                .and_then(Clone::clone)
                .unwrap_or_default();
            let payload = if let Some(record) = ingress {
                state
                    .apply_token_ingress_execution(&record, &pre, &post)
                    .map_err(|error| self.v2_invalid_transition(error))?;
                TransitionPayloadV2::TokenIngress {
                    record,
                    transaction: wire,
                    pre,
                    post,
                    execution_result,
                    replay,
                }
            } else {
                state
                    .apply_execution(&pre, &post)
                    .map_err(|error| self.v2_invalid_transition(error))?;
                TransitionPayloadV2::Execution {
                    transaction: wire,
                    pre,
                    post,
                    execution_result,
                    replay,
                }
            };
            let mut readonly_l1_values = Vec::new();
            if let Some(executed) = processed.executed_transaction() {
                for (key, value) in &executed.loaded_transaction.accounts {
                    if !state.accounts().contains_key(&key.to_bytes()) && !outputs.contains_key(key)
                    {
                        readonly_l1_values.push(ReadonlyL1ValueV1 {
                            account: *key,
                            owner: *value.owner(),
                            lamports: value.lamports(),
                            data_hash: hashv(&[value.data()]).to_bytes(),
                            observed_l1_slot: self.sync_status.latest_l1_slot(),
                        });
                    }
                }
            }
            readonly_l1_values.sort_unstable_by_key(|value| value.account);
            readonly_l1_values.dedup_by_key(|value| value.account);
            let settlement_effects = if status.is_ok() && ingress.is_none() {
                self.v2_settlement_effects(bank, transaction, &output.inner, &mut state)?
            } else {
                vec![]
            };
            records.push(RecordedTransitionV2 {
                er_slot: bank.slot(),
                before,
                after: state
                    .opening()
                    .map_err(|error| self.v2_persistence_failure(error))?,
                payload,
                readonly_l1_values,
                settlement_effects,
            });
        }
        store.append_batch(records).map_err(|error| {
            if store.is_poisoned() {
                return self.v2_persistence_failure(error);
            }
            match error.kind() {
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::AlreadyExists => {
                    solana_transaction::TransactionError::CommitCancelled
                }
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::InvalidInput => {
                    self.v2_invalid_transition(error)
                }
                _ => self.v2_persistence_failure(error),
            }
        })
    }

    pub(super) fn refresh_v2_payout_cache(
        &self,
    ) -> Result<(), solana_transaction::TransactionError> {
        let Some(store) = self.lineage_store_v2() else {
            return Ok(());
        };
        let (sol, tokens) = store
            .lock()
            .unwrap()
            .pending_payout_events()
            .map_err(|error| self.v2_persistence_failure(error))?;
        *self.withdrawal_payout_events.write().unwrap() = sol;
        *self.token_withdrawal_payout_events.write().unwrap() = tokens;
        Ok(())
    }

    fn v2_persistence_failure(
        &self,
        error: impl std::fmt::Display,
    ) -> solana_transaction::TransactionError {
        self.active.store(false, Ordering::Relaxed);
        self.sync_status
            .set_checkpoint_error(Some("checkpoint_lineage_persistence_failed"));
        warn!("Versioned checkpoint persistence failed before SVM commit: {error}");
        solana_transaction::TransactionError::CommitCancelled
    }

    fn v2_invalid_transition(
        &self,
        error: impl std::fmt::Display,
    ) -> solana_transaction::TransactionError {
        warn!("Versioned ER transition rejected before SVM commit: {error}");
        solana_transaction::TransactionError::CommitCancelled
    }

    fn v2_settlement_effects(
        &self,
        bank: &Bank,
        transaction: &VersionedTransaction,
        inner: &[RecordedInnerInstructionV2],
        state: &mut CommittedStateV2,
    ) -> Result<Vec<Vec<u8>>, solana_transaction::TransactionError> {
        use borsh::BorshDeserialize;
        let addresses = Self::load_transaction_addresses(bank, transaction).unwrap_or_default();
        let keys = Self::full_account_keys(transaction, &addresses);
        let mut effects = Vec::new();
        for (outer, instruction) in transaction.message.instructions().iter().enumerate() {
            let Some(program) = keys.get(instruction.program_id_index as usize) else {
                continue;
            };
            let effect = if program == &self.portal_program_id
                && instruction.data.len() == 9
                && instruction.data[0] == 13
                && instruction.accounts.len() >= 5
            {
                let indices = &instruction.accounts;
                let (Some(source), Some(recipient), Some(sink), Some(system), Some(clock)) = (
                    keys.get(indices[0] as usize),
                    keys.get(indices[1] as usize),
                    keys.get(indices[2] as usize),
                    keys.get(indices[3] as usize),
                    keys.get(indices[4] as usize),
                ) else {
                    return Err(solana_transaction::TransactionError::CommitCancelled);
                };
                let amount = u64::from_le_bytes(
                    instruction.data[1..]
                        .try_into()
                        .map_err(|_| solana_transaction::TransactionError::CommitCancelled)?,
                );
                if !transaction.message.is_signer(indices[0] as usize)
                    || *sink != crate::WITHDRAWAL_SINK
                    || !system_program::check_id(system)
                    || *clock != sysvar::clock::id()
                    || !inner.iter().any(|inner| {
                        inner.outer_index as usize == outer
                            && keys
                                .get(inner.program_id_index as usize)
                                .is_some_and(system_program::check_id)
                            && inner.accounts.len() >= 2
                            && Self::system_transfer_lamports(&inner.data) == Some(amount)
                            && keys.get(inner.accounts[0] as usize) == Some(source)
                            && keys.get(inner.accounts[1] as usize) == Some(sink)
                    })
                {
                    return Err(solana_transaction::TransactionError::CommitCancelled);
                }
                SettlementEffectV2::SolWithdrawal {
                    source: source.to_bytes(),
                    recipient: recipient.to_bytes(),
                    amount,
                    cumulative_withdrawn: state
                        .withdrawn(source.to_bytes())
                        .checked_add(amount)
                        .ok_or(solana_transaction::TransactionError::CommitCancelled)?,
                }
            } else {
                let Ok(
                    northstar_token_bridge::instruction::TokenBridgeInstruction::StartWithdrawal {
                        amount,
                        decimals,
                    },
                ) = borsh::from_slice(&instruction.data)
                else {
                    continue;
                };
                if instruction.accounts.len() < 6 {
                    return Err(solana_transaction::TransactionError::CommitCancelled);
                }
                let (Some(owner), Some(account), Some(bridge), Some(portal), Some(destination)) = (
                    keys.get(instruction.accounts[0] as usize),
                    keys.get(instruction.accounts[1] as usize),
                    keys.get(instruction.accounts[2] as usize),
                    keys.get(instruction.accounts[3] as usize),
                    keys.get(instruction.accounts[4] as usize),
                ) else {
                    return Err(solana_transaction::TransactionError::CommitCancelled);
                };
                let registration = bank
                    .get_account(bridge)
                    .ok_or(solana_transaction::TransactionError::CommitCancelled)?;
                let registration_state: northstar_portal::SessionBridge =
                    borsh::from_slice(registration.data())
                        .map_err(|_| solana_transaction::TransactionError::CommitCancelled)?;
                let value = state
                    .accounts()
                    .get(&account.to_bytes())
                    .ok_or(solana_transaction::TransactionError::CommitCancelled)?;
                let token =
                    northstar_token_bridge::state::ErTokenAccount::try_from_slice(&value.data)
                        .map_err(|_| solana_transaction::TransactionError::CommitCancelled)?;
                if !registration_state.is_valid()
                    || registration.owner() != &self.portal_program_id
                    || registration_state.bridge_program != *program
                    || *portal != self.portal_program_id
                    || value.owner != program.to_bytes()
                    || !token.is_valid()
                    || token.owner != owner.to_bytes()
                    || token.session_bridge != bridge.to_bytes()
                    || token.mint != registration_state.mint.to_bytes()
                    || !transaction
                        .message
                        .is_signer(instruction.accounts[0] as usize)
                {
                    return Err(solana_transaction::TransactionError::CommitCancelled);
                }
                SettlementEffectV2::TokenWithdrawal {
                    bridge: program.to_bytes(),
                    session_bridge: bridge.to_bytes(),
                    account: account.to_bytes(),
                    destination: destination.to_bytes(),
                    amount,
                    decimals,
                    cumulative_withdrawn: state
                        .withdrawn(account.to_bytes())
                        .checked_add(amount)
                        .ok_or(
                        solana_transaction::TransactionError::CommitCancelled,
                    )?,
                }
            };
            let encoded = borsh::to_vec(&effect)
                .map_err(|_| solana_transaction::TransactionError::CommitCancelled)?;
            state
                .apply_settlement_effects(std::slice::from_ref(&encoded))
                .map_err(|error| self.v2_invalid_transition(error))?;
            effects.push(encoded);
        }
        Ok(effects)
    }
}
