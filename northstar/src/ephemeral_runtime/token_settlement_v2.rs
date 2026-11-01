use {
    super::*, northstar_portal::SessionBridgeV2,
    spl_token_interface::state::Account as SplTokenAccount, std::io,
};

fn invalid_binding() -> io::Error {
    io::Error::other("token settlement metadata does not match committed session")
}

impl EphemeralRuntime {
    pub(crate) fn settlement_token_withdrawals_v2(
        &self,
        through_er_slot: Slot,
    ) -> io::Result<Vec<TokenWithdrawalSettlement>> {
        let store = self.lineage_store_v2().ok_or_else(invalid_binding)?;
        let identity = *store.lock().unwrap().accepted_state().identity();
        let context = identity
            .commitment(crate::continuity_v2::hash_parts)
            .map_err(|_| invalid_binding())?;
        let mut events = self
            .token_withdrawal_payout_events
            .read()
            .unwrap()
            .iter()
            .filter(|event| event.er_slot <= through_er_slot)
            .cloned()
            .collect::<Vec<_>>();
        events.sort_by(|a, b| {
            a.er_slot
                .cmp(&b.er_slot)
                .then_with(|| a.signature.to_string().cmp(&b.signature.to_string()))
                .then_with(|| {
                    a.er_token_account
                        .to_bytes()
                        .cmp(&b.er_token_account.to_bytes())
                })
        });
        let mut cumulative_by_bridge = HashMap::<Pubkey, u64>::new();
        let mut settlements = Vec::new();
        for event in events {
            let physical = self
                .l1_anchor_bank
                .get_account(&event.session_bridge)
                .ok_or_else(invalid_binding)?;
            let registry: SessionBridgeV2 =
                borsh::from_slice(physical.data()).map_err(|_| invalid_binding())?;
            let (key, bump) = northstar_portal::find_session_bridge_v2_pda(
                &self.portal_program_id,
                &context,
                &registry.base.mint,
            );
            if physical.owner() != &self.portal_program_id
                || physical.executable()
                || !registry.is_valid()
                || registry.session_context != context
                || registry.base.session.to_bytes() != identity.session
                || registry.base.bridge_program != event.bridge_program
                || key != event.session_bridge
                || registry.base.bump != bump
            {
                return Err(invalid_binding());
            }
            let bridge = registry.base;
            let mint = self
                .l1_anchor_bank
                .get_account(&bridge.mint)
                .ok_or_else(invalid_binding)?;
            if mint.owner() != &bridge.token_program || mint.executable() {
                return Err(invalid_binding());
            }
            let mint_state = Mint::unpack(mint.data()).map_err(|_| invalid_binding())?;
            if !mint_state.is_initialized || mint_state.decimals != event.decimals {
                return Err(invalid_binding());
            }
            let (vault, bump) = northstar_token_bridge::find_token_vault_pda(
                &event.bridge_program,
                &event.session_bridge,
            );
            if vault != bridge.vault {
                return Err(invalid_binding());
            }
            let physical = self
                .l1_anchor_bank
                .get_account(&vault)
                .ok_or_else(invalid_binding)?;
            let state: northstar_token_bridge::state::TokenVault =
                borsh::from_slice(physical.data()).map_err(|_| invalid_binding())?;
            if physical.owner() != &event.bridge_program
                || physical.executable()
                || !state.is_valid()
                || state.bump != bump
                || state.session_bridge != event.session_bridge.to_bytes()
                || state.mint != bridge.mint.to_bytes()
                || state.token_program != bridge.token_program.to_bytes()
                || state.withdrawn > state.deposited
            {
                return Err(invalid_binding());
            }
            let destination = self
                .l1_anchor_bank
                .get_account(&event.l1_destination_token_account)
                .ok_or_else(invalid_binding)?;
            let destination_state =
                SplTokenAccount::unpack(destination.data()).map_err(|_| invalid_binding())?;
            if destination.owner() != &bridge.token_program
                || destination.executable()
                || destination_state.mint.to_bytes() != bridge.mint.to_bytes()
            {
                return Err(invalid_binding());
            }
            let cumulative = cumulative_by_bridge
                .entry(event.session_bridge)
                .or_insert(state.withdrawn);
            *cumulative = cumulative
                .checked_add(event.amount)
                .ok_or_else(invalid_binding)?;
            if event.amount == 0 || *cumulative > state.deposited {
                return Err(invalid_binding());
            }
            settlements.push(TokenWithdrawalSettlement {
                bridge_program: event.bridge_program,
                session_bridge: event.session_bridge,
                er_token_account: event.er_token_account,
                vault,
                vault_token_account: Pubkey::new_from_array(state.vault_token_account),
                l1_destination_token_account: event.l1_destination_token_account,
                mint: bridge.mint,
                token_program: bridge.token_program,
                amount: event.amount,
                withdrawn: *cumulative,
                decimals: mint_state.decimals,
            });
        }
        Ok(settlements)
    }

    pub(super) fn token_payouts_complete_v2(
        &self,
        l1_root: &Bank,
        checkpoint: Pubkey,
        withdrawals: &[TokenWithdrawalSettlement],
    ) -> io::Result<bool> {
        for withdrawal in withdrawals {
            let physical = l1_root
                .get_account(&withdrawal.vault)
                .ok_or_else(invalid_binding)?;
            let vault: northstar_token_bridge::state::TokenVault =
                borsh::from_slice(physical.data()).map_err(|_| invalid_binding())?;
            if physical.owner() != &withdrawal.bridge_program
                || physical.executable()
                || !vault.is_valid()
                || vault.session_bridge != withdrawal.session_bridge.to_bytes()
                || vault.mint != withdrawal.mint.to_bytes()
                || vault.vault_token_account != withdrawal.vault_token_account.to_bytes()
                || vault.token_program != withdrawal.token_program.to_bytes()
            {
                return Err(invalid_binding());
            }
            if vault.withdrawn < withdrawal.withdrawn {
                return Ok(false);
            }
            let (key, bump) = northstar_portal::find_token_withdrawal_authorization_pda(
                &self.portal_program_id,
                &checkpoint,
                &withdrawal.vault,
                withdrawal.withdrawn,
            );
            let Some(physical) = l1_root.get_account(&key) else {
                return Ok(false);
            };
            let authorization: northstar_portal::TokenWithdrawalAuthorization =
                borsh::from_slice(physical.data()).map_err(|_| invalid_binding())?;
            // Payouts consume this tuple and advance the vault, not the deposit receipt's counter.
            let expected = solana_sha256_hasher::hashv(&[
                b"northstar-token-withdrawal-authorization-v0",
                withdrawal.bridge_program.as_ref(),
                withdrawal.session_bridge.as_ref(),
                withdrawal.er_token_account.as_ref(),
                withdrawal.vault.as_ref(),
                withdrawal.vault_token_account.as_ref(),
                withdrawal.l1_destination_token_account.as_ref(),
                withdrawal.mint.as_ref(),
                withdrawal.token_program.as_ref(),
                &withdrawal.amount.to_le_bytes(),
                &withdrawal.withdrawn.to_le_bytes(),
                &[withdrawal.decimals],
            ])
            .to_bytes();
            if physical.owner() != &self.portal_program_id
                || physical.executable()
                || !authorization.is_valid()
                || authorization.bump != bump
                || authorization.checkpoint != checkpoint
                || authorization.tuple_hash != expected
            {
                return Err(invalid_binding());
            }
            if !authorization.consumed {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
