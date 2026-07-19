use borsh::{BorshDeserialize, BorshSerialize};

#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub enum TokenBridgeInstruction {
    InitializeVault,
    InitializeErTokenAccount {
        owner: [u8; 32],
    },
    Deposit {
        amount: u64,
        decimals: u8,
    },
    Transfer {
        amount: u64,
    },
    Withdraw {
        amount: u64,
        decimals: u8,
    },
    DelegateErTokenAccount {
        grid_id: u64,
    },
    UndelegateErTokenAccount,
    StartWithdrawal {
        amount: u64,
        decimals: u8,
    },
    SettleWithdrawal {
        er_slot: u64,
        checksum: [u8; 32],
        amount: u64,
        withdrawn: u64,
        decimals: u8,
    },
}
