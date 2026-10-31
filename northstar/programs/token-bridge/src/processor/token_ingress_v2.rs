use {
    super::{AccountInfo, ProgramError, ProgramResult, Pubkey, TokenVault},
    pinocchio::{
        cpi::{invoke_signed, Seed, Signer},
        instruction::{InstructionAccount as AccountMeta, InstructionView as Instruction},
        sysvars::{clock::Clock, Sysvar},
    },
};

pub(super) struct DepositIngressV2<'a> {
    pub payer: &'a AccountInfo,
    pub system: &'a AccountInfo,
    pub session: &'a AccountInfo,
    pub registry: &'a AccountInfo,
    pub vault: &'a AccountInfo,
    pub er: &'a AccountInfo,
    pub delegation: &'a AccountInfo,
    pub receipt: &'a AccountInfo,
    pub origin: &'a AccountInfo,
    pub ingress: &'a AccountInfo,
    pub portal: &'a AccountInfo,
}

pub(super) fn validate_context(
    program_id: &Pubkey,
    portal: &AccountInfo,
    session: &AccountInfo,
    registry: &AccountInfo,
    vault: &AccountInfo,
) -> ProgramResult {
    if !portal.executable() || !session.owned_by(portal.address()) || session.executable() {
        return Err(ProgramError::InvalidAccountOwner);
    }
    let registration = crate::portal_abi::registration_v2(
        program_id,
        portal.address(),
        registry.address(),
        &registry.try_borrow()?,
    )?;
    let frame = crate::portal_abi::session_v2(
        &registration,
        portal.address(),
        session.address(),
        &session.try_borrow()?,
    )?;
    if frame.base.is_expired(Clock::get()?.slot) || *vault.address() != registration.base.vault {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

impl DepositIngressV2<'_> {
    pub(super) fn append(&self, vault_bump: u8, amount: u64) -> ProgramResult {
        let mut data = [0u8; 9];
        data[0] = 37;
        data[1..].copy_from_slice(&amount.to_le_bytes());
        let accounts = [
            AccountMeta::writable_signer(self.payer.address()),
            AccountMeta::readonly(self.system.address()),
            AccountMeta::writable(self.session.address()),
            AccountMeta::readonly(self.registry.address()),
            AccountMeta::readonly_signer(self.vault.address()),
            AccountMeta::readonly(self.er.address()),
            AccountMeta::readonly(self.delegation.address()),
            AccountMeta::readonly(self.receipt.address()),
            AccountMeta::readonly(self.origin.address()),
            AccountMeta::writable(self.ingress.address()),
        ];
        let instruction = Instruction {
            program_id: self.portal.address(),
            accounts: &accounts,
            data: &data,
        };
        let bump = [vault_bump];
        let seeds = [
            Seed::from(TokenVault::SEED_PREFIX),
            Seed::from(self.registry.address().as_ref()),
            Seed::from(&bump),
        ];
        invoke_signed(
            &instruction,
            &[
                self.payer,
                self.system,
                self.session,
                self.registry,
                self.vault,
                self.er,
                self.delegation,
                self.receipt,
                self.origin,
                self.ingress,
            ],
            &[Signer::from(&seeds)],
        )
    }
}
