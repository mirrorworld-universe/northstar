use {
    crate::{
        accounts::{Request, config_address, discriminator, request_address},
        protocol,
    },
    anyhow::{Result, ensure},
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};
pub fn fulfill(
    program: Pubkey,
    verifier: Pubkey,
    fulfiller: Pubkey,
    address: Pubkey,
    request: &Request,
    proof: &[u8; 80],
) -> Result<Instruction> {
    ensure!(request.status == 0, "request is not pending");
    ensure!(
        !request.callback_accounts.iter().any(|a| a.key == fulfiller),
        "fulfiller cannot be a callback account"
    );
    let (signer, consumer) = if request.mode == 0 {
        (
            Pubkey::find_program_address(&[b"vrf_callback", request.requester.as_ref()], &program)
                .0,
            request.requester,
        )
    } else {
        (program, program)
    };
    let mut accounts = vec![
        AccountMeta::new(fulfiller, true),
        AccountMeta::new_readonly(config_address(&program).0, false),
        AccountMeta::new(address, false),
        AccountMeta::new(request.rent_payer, false),
        AccountMeta::new_readonly(signer, false),
        AccountMeta::new_readonly(consumer, false),
        AccountMeta::new_readonly(verifier, false),
    ];
    if request.mode == 0 {
        accounts.extend(request.callback_accounts.iter().map(|a| AccountMeta {
            pubkey: a.key,
            is_signer: false,
            is_writable: a.writable,
        }));
    }
    Ok(Instruction {
        program_id: program,
        accounts,
        data: protocol::fulfill_data(proof).to_vec(),
    })
}
pub fn compute_limit(units: u32) -> Result<Instruction> {
    ensure!((1..=1_400_000).contains(&units), "invalid compute budget");
    let mut data = vec![2];
    data.extend(units.to_le_bytes());
    Ok(Instruction {
        program_id: "ComputeBudget111111111111111111111111111111".parse()?,
        accounts: vec![],
        data,
    })
}
pub fn store_request(
    program: Pubkey,
    payer: Pubkey,
    request_key: [u8; 32],
    seed: [u8; 32],
    deadline: u16,
) -> Result<Instruction> {
    ensure!((2..=1000).contains(&deadline), "invalid deadline");
    let mut data = discriminator("global", "request_random").to_vec();
    data.extend(request_key);
    data.extend(seed);
    data.extend([1, 1]);
    data.extend(deadline.to_le_bytes());
    data.push(0);
    Ok(Instruction {
        program_id: program,
        accounts: vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(payer, false),
            AccountMeta::new_readonly(payer, true),
            AccountMeta::new_readonly(config_address(&program).0, false),
            AccountMeta::new(request_address(&program, &payer, &request_key).0, false),
            AccountMeta::new_readonly(
                "SysvarS1otHashes111111111111111111111111111".parse()?,
                false,
            ),
            AccountMeta::new_readonly(Pubkey::default(), false),
        ],
        data,
    })
}
