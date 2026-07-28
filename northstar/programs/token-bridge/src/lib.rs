pub mod instruction;
pub mod state;

#[cfg(target_os = "solana")]
mod processor;
#[cfg(target_os = "solana")]
pub use processor::*;

#[cfg(not(target_os = "solana"))]
mod host {
    use {
        crate::state::{BridgeBuffer, ErTokenAccount, TokenDepositReceipt, TokenVault},
        solana_pubkey::Pubkey,
    };

    solana_pubkey::declare_id!("HeVLVaSa9WnFai9aTRJ3UR2c4jwbMe5nbjagmDP1GbXR");

    pub fn find_token_vault_pda(program_id: &Pubkey, session_bridge: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[TokenVault::SEED_PREFIX, session_bridge.as_ref()],
            program_id,
        )
    }

    pub fn find_er_token_account_pda(
        program_id: &Pubkey,
        session_bridge: &Pubkey,
        owner: &Pubkey,
    ) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[
                ErTokenAccount::SEED_PREFIX,
                session_bridge.as_ref(),
                owner.as_ref(),
            ],
            program_id,
        )
    }

    pub fn find_token_deposit_receipt_pda(
        program_id: &Pubkey,
        session_bridge: &Pubkey,
        er_token_account: &Pubkey,
    ) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[
                TokenDepositReceipt::SEED_PREFIX,
                session_bridge.as_ref(),
                er_token_account.as_ref(),
            ],
            program_id,
        )
    }

    pub fn find_buffer_pda(program_id: &Pubkey, er_token_account: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[BridgeBuffer::SEED_PREFIX, er_token_account.as_ref()],
            program_id,
        )
    }
}

#[cfg(not(target_os = "solana"))]
pub use host::*;
