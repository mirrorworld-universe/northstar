use {
    super::EphemeralRuntime,
    northstar_token_bridge::{
        instruction::TokenBridgeInstruction,
        state::{ErTokenAccount, TokenDepositProgress},
    },
    solana_account::ReadableAccount,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
    solana_signer::Signer,
    solana_transaction::Transaction,
};

impl EphemeralRuntime {
    pub(crate) fn process_token_deposits(&self) {
        if !self.is_active() || self.checkpoint_artifact_v1().is_some() {
            return;
        }
        let mut accounts = self
            .delegated_accounts
            .read()
            .unwrap()
            .iter()
            .copied()
            .collect::<Vec<_>>();
        accounts.sort_unstable();
        for key in accounts {
            let Some(account) = self.bank().get_account(&key) else {
                continue;
            };
            let Ok(state) = borsh::from_slice::<ErTokenAccount>(account.data()) else {
                continue;
            };
            if !state.is_valid() {
                continue;
            }
            let bridge = Pubkey::new_from_array(state.session_bridge);
            let receipt = northstar_token_bridge::find_token_deposit_receipt_pda(
                account.owner(),
                &bridge,
                &key,
            )
            .0;
            let Some(receipt) = self.l1_anchor_bank.get_account(&receipt) else {
                continue;
            };
            let Ok(receipt) = borsh::from_slice::<northstar_token_bridge::state::TokenDepositReceipt>(
                receipt.data(),
            ) else {
                continue;
            };
            self.credit_token_deposit(account.owner(), &bridge, &key, receipt.balance);
        }
    }

    pub fn credit_token_deposit(
        &self,
        program: &Pubkey,
        bridge: &Pubkey,
        account: &Pubkey,
        balance: u64,
    ) -> bool {
        if !self.is_active()
            || !self.delegated_accounts.read().unwrap().contains(account)
            || self.checkpoint_artifact_v1().is_some()
        {
            return false;
        }
        let bank = self.bank();
        let origin = Pubkey::find_program_address(
            &[TokenDepositProgress::ORIGIN_SEED, account.as_ref()],
            program,
        )
        .0;
        let Some(origin_account) = self.l1_anchor_bank.get_account(&origin) else {
            return false;
        };
        if origin_account.owner() != program {
            return false;
        }
        let Ok(origin_state) = borsh::from_slice::<TokenDepositProgress>(origin_account.data())
        else {
            return false;
        };
        if origin_state.discriminator != TokenDepositProgress::ORIGIN_DISCRIMINATOR {
            return false;
        }
        let (cursor, cursor_bump) = Pubkey::find_program_address(
            &[
                TokenDepositProgress::CURSOR_SEED,
                account.as_ref(),
                &origin_state.balance.to_le_bytes(),
            ],
            program,
        );
        let credited = if let Some(value) = bank.get_account(&cursor) {
            if value.owner() == &solana_sdk_ids::system_program::id() && value.data().is_empty() {
                origin_state.balance
            } else {
                let Ok(progress) = borsh::from_slice::<TokenDepositProgress>(value.data()) else {
                    return false;
                };
                if value.owner() != program
                    || progress.discriminator != TokenDepositProgress::CURSOR_DISCRIMINATOR
                    || progress.bump != cursor_bump
                    || progress.balance < origin_state.balance
                {
                    return false;
                }
                progress.balance
            }
        } else {
            origin_state.balance
        };
        if balance <= credited {
            return true;
        }
        let Some(session) = *self.session_pda.read().unwrap() else {
            return false;
        };
        let payer = self.manager_keypair.pubkey();
        if bank.get_account(&payer).is_none() {
            return false;
        }
        let receipt =
            northstar_token_bridge::find_token_deposit_receipt_pda(program, bridge, account).0;
        let delegation =
            northstar_portal::find_delegation_record_pda(&self.portal_program_id, account).0;
        let instruction = Instruction {
            program_id: *program,
            accounts: vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(*account, false),
                AccountMeta::new_readonly(*bridge, false),
                AccountMeta::new_readonly(self.portal_program_id, false),
                AccountMeta::new_readonly(session, false),
                AccountMeta::new_readonly(delegation, false),
                AccountMeta::new_readonly(receipt, false),
                AccountMeta::new_readonly(origin, false),
                AccountMeta::new(cursor, false),
                AccountMeta::new_readonly(solana_sdk_ids::system_program::id(), false),
            ],
            data: borsh::to_vec(&TokenBridgeInstruction::ApplyTokenDeposit { balance }).unwrap(),
        };
        let transaction = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&payer),
            &[self.manager_keypair.as_ref()],
            bank.last_blockhash(),
        );
        self._tx_client.send_token_deposit_transaction(
            bincode::serialize(&transaction).unwrap(),
            *account,
            payer,
            cursor,
        );
        self.bank()
            .get_account(&cursor)
            .and_then(|value| borsh::from_slice::<TokenDepositProgress>(value.data()).ok())
            .is_some_and(|value| value.balance >= balance)
    }
}

impl EphemeralRuntime {
    pub(super) fn has_pending_token_deposit(&self, account: &Pubkey) -> bool {
        let bank = self.bank();
        let Some(value) = bank.get_account(account) else {
            return false;
        };
        let Ok(state) = borsh::from_slice::<ErTokenAccount>(value.data()) else {
            return false;
        };
        if !state.is_valid() {
            return false;
        }
        let program = value.owner();
        let bridge = Pubkey::new_from_array(state.session_bridge);
        let receipt =
            northstar_token_bridge::find_token_deposit_receipt_pda(program, &bridge, account).0;
        let Some(receipt) = self.l1_anchor_bank.get_account(&receipt) else {
            return false;
        };
        if receipt.owner() != program {
            return true;
        }
        let Ok(receipt) =
            borsh::from_slice::<northstar_token_bridge::state::TokenDepositReceipt>(receipt.data())
        else {
            return true;
        };
        let origin = Pubkey::find_program_address(
            &[TokenDepositProgress::ORIGIN_SEED, account.as_ref()],
            program,
        )
        .0;
        let Some(origin) = self.l1_anchor_bank.get_account(&origin) else {
            return receipt.balance != 0;
        };
        let Ok(baseline) = borsh::from_slice::<TokenDepositProgress>(origin.data()) else {
            return true;
        };
        if origin.owner() != program
            || baseline.discriminator != TokenDepositProgress::ORIGIN_DISCRIMINATOR
        {
            return true;
        }
        let (cursor, bump) = Pubkey::find_program_address(
            &[
                TokenDepositProgress::CURSOR_SEED,
                account.as_ref(),
                &baseline.balance.to_le_bytes(),
            ],
            program,
        );
        let credited = if let Some(cursor) = bank.get_account(&cursor) {
            if cursor.owner() == &solana_sdk_ids::system_program::id() && cursor.data().is_empty() {
                baseline.balance
            } else {
                let Ok(progress) = borsh::from_slice::<TokenDepositProgress>(cursor.data()) else {
                    return true;
                };
                if cursor.owner() != program
                    || progress.discriminator != TokenDepositProgress::CURSOR_DISCRIMINATOR
                    || progress.bump != bump
                {
                    return true;
                }
                progress.balance
            }
        } else {
            baseline.balance
        };
        credited != receipt.balance
    }
}
