use {
    crate::protocol::AlphaInput,
    anyhow::{Result, bail, ensure},
    sha2::{Digest, Sha256},
    solana_pubkey::Pubkey,
};
pub fn discriminator(namespace: &str, name: &str) -> [u8; 8] {
    Sha256::digest(format!("{namespace}:{name}").as_bytes())[..8]
        .try_into()
        .unwrap()
}
pub fn config_address(program: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"config"], program)
}
pub fn request_address(program: &Pubkey, requester: &Pubkey, key: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"request", requester.as_ref(), key], program)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallbackAccount {
    pub key: Pubkey,
    pub writable: bool,
}
#[derive(Clone, Debug)]
pub struct Request {
    pub status: u8,
    pub mode: u8,
    pub requester: Pubkey,
    pub request_key: [u8; 32],
    pub seed: [u8; 32],
    pub request_slot: u64,
    pub hash_slot: u64,
    pub slot_hash: [u8; 32],
    pub alpha: [u8; 32],
    pub vrf_key: [u8; 32],
    pub deadline_slot: u64,
    pub fee: u64,
    pub fee_payer: Pubkey,
    pub rent_payer: Pubkey,
    pub callback_discriminator: [u8; 8],
    pub callback_compute: u32,
    pub output: [u8; 64],
    pub fulfilled_slot: u64,
    pub callback_accounts: Vec<CallbackAccount>,
    pub callback_args: Vec<u8>,
}
impl Request {
    pub fn decode(program: &Pubkey, address: &Pubkey, owner: &Pubkey, data: &[u8]) -> Result<Self> {
        ensure!(owner == program, "request owner mismatch");
        ensure!(
            (401..=993).contains(&data.len()),
            "request size out of bounds"
        );
        let mut r = Reader(data);
        ensure!(
            r.array::<8>()? == discriminator("account", "RequestAccount"),
            "request discriminator mismatch"
        );
        ensure!(r.byte()? == 1, "unsupported request version");
        let status = r.byte()?;
        let mode = r.byte()?;
        ensure!(status <= 1 && mode <= 1, "invalid request enum");
        r.array::<8>()?;
        let requester = r.key()?;
        let request_key = r.array()?;
        let seed = r.array()?;
        let request_slot = r.u64()?;
        let hash_slot = r.u64()?;
        let slot_hash = r.array()?;
        let alpha = r.array()?;
        let vrf_key = r.array()?;
        ensure!(r.byte()? == 1, "unsupported VRF suite");
        let deadline_slot = r.u64()?;
        let fee = r.u64()?;
        let fee_payer = r.key()?;
        let rent_payer = r.key()?;
        let callback_discriminator = r.array()?;
        let callback_compute = r.u32()?;
        let output = r.array()?;
        let fulfilled_slot = r.u64()?;
        let bump = r.byte()?;
        let count = r.u32()? as usize;
        ensure!(count <= 16, "callback account limit");
        let mut callback_accounts = Vec::with_capacity(count);
        for _ in 0..count {
            let key = r.key()?;
            let writable = r.byte()?;
            ensure!(writable <= 1, "invalid callback bool");
            callback_accounts.push(CallbackAccount {
                key,
                writable: writable == 1,
            });
        }
        let count = r.u32()? as usize;
        ensure!(count <= 64, "callback argument limit");
        let callback_args = r.take(count)?.to_vec();
        ensure!(r.0.is_empty(), "trailing request bytes");
        ensure!(
            request_address(program, &requester, &request_key) == (*address, bump),
            "request PDA mismatch"
        );
        ensure!(
            hash_slot <= request_slot && deadline_slot > request_slot,
            "invalid request slots"
        );
        let expected = AlphaInput {
            program_id: program.to_bytes(),
            request_address: address.to_bytes(),
            seed,
            request_slot,
            hash_slot,
            slot_hash,
        }
        .alpha();
        ensure!(alpha == expected, "request alpha mismatch");
        ensure!(callback_compute <= 1_200_000, "callback compute limit");
        if mode == 1 {
            ensure!(
                callback_accounts.is_empty()
                    && callback_args.is_empty()
                    && callback_compute == 0
                    && callback_discriminator == [0; 8],
                "Store request has callback fields"
            );
        }
        if status == 0 {
            ensure!(
                output == [0; 64] && fulfilled_slot == 0,
                "pending request has fulfillment fields"
            );
        }
        Ok(Self {
            status,
            mode,
            requester,
            request_key,
            seed,
            request_slot,
            hash_slot,
            slot_hash,
            alpha,
            vrf_key,
            deadline_slot,
            fee,
            fee_payer,
            rent_payer,
            callback_discriminator,
            callback_compute,
            output,
            fulfilled_slot,
            callback_accounts,
            callback_args,
        })
    }
}
#[derive(Debug)]
pub struct ProgramConfig {
    pub main_authority: Pubkey,
    pub pending_main_authority: Option<Pubkey>,
    pub operations_authority: Pubkey,
    pub status: u8,
    pub vrf_key: [u8; 32],
    pub suite: u8,
    pub fulfillers: Vec<Pubkey>,
}
impl ProgramConfig {
    pub fn decode(program: &Pubkey, address: &Pubkey, owner: &Pubkey, data: &[u8]) -> Result<Self> {
        ensure!(
            program == owner && data.len() == 360,
            "invalid config owner/size"
        );
        let mut r = Reader(data);
        ensure!(
            r.array::<8>()? == discriminator("account", "ProgramConfigAccount"),
            "config discriminator mismatch"
        );
        ensure!(r.byte()? == 1, "unsupported config version");
        r.take(8)?;
        let main_authority = r.key()?;
        let pending_main_authority = match r.byte()? {
            0 => None,
            1 => Some(r.key()?),
            _ => bail!("invalid config authority option"),
        };
        let operations_authority = r.key()?;
        let status = r.byte()?;
        ensure!(status <= 2, "invalid config status");
        let vrf_key = r.array()?;
        let suite = r.byte()?;
        let mut fulfillers = Vec::with_capacity(4);
        for _ in 0..4 {
            fulfillers.push(r.key()?);
        }
        let count = r.byte()? as usize;
        ensure!(count <= 4, "invalid fulfiller count");
        fulfillers.truncate(count);
        r.take(8 + 2 + 2 + 2 + 4)?;
        let bump = r.byte()?;
        ensure!(
            config_address(program) == (*address, bump),
            "config PDA mismatch"
        );
        Ok(Self {
            main_authority,
            pending_main_authority,
            operations_authority,
            status,
            vrf_key,
            suite,
            fulfillers,
        })
    }
}
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        ensure!(len <= self.0.len(), "truncated account");
        let (value, remaining) = self.0.split_at(len);
        self.0 = remaining;
        Ok(value)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into()?)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    fn key(&mut self) -> Result<Pubkey> {
        Ok(Pubkey::new_from_array(self.array()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
}
