use borsh::{BorshDeserialize, BorshSerialize};

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub enum TokenBridgeInstruction {
    InitializeVault,
    InitializeErTokenAccount { owner: [u8; 32] },
    Deposit { amount: u64, decimals: u8 },
    Transfer { amount: u64 },
    Withdraw { amount: u64, decimals: u8 },
    DelegateErTokenAccount { grid_id: u64 },
    UndelegateErTokenAccount,
}
