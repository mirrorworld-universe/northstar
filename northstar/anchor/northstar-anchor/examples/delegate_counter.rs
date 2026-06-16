use anchor_lang::prelude::*;
use northstar_anchor::{cpi::DelegateConfig, delegate};

declare_id!("11111111111111111111111111111111");

pub const COUNTER_SEED: &[u8] = b"counter";

pub fn delegate_counter(ctx: Context<DelegateCounter>, grid_id: u64) -> Result<()> {
    let payer = ctx.accounts.payer.key();
    let seeds: &[&[u8]] = &[COUNTER_SEED, payer.as_ref()];
    ctx.accounts
        .delegate_counter(&ctx.accounts.payer, seeds, DelegateConfig { grid_id })?;
    Ok(())
}

pub fn undelegate_counter(ctx: Context<DelegateCounter>) -> Result<()> {
    let payer = ctx.accounts.payer.key();
    let seeds: &[&[u8]] = &[COUNTER_SEED, payer.as_ref()];
    ctx.accounts
        .undelegate_counter(&ctx.accounts.payer, seeds)?;
    Ok(())
}

#[delegate]
#[derive(Accounts)]
pub struct DelegateCounter<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: PDA verified by `delegate_counter`/`undelegate_counter` seeds.
    #[account(mut, del)]
    pub counter: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct Increment<'info> {
    #[account(mut, seeds = [COUNTER_SEED, payer.key().as_ref()], bump)]
    pub counter: Account<'info, Counter>,
    pub payer: Signer<'info>,
}

#[account]
pub struct Counter {
    pub count: u64,
}

fn main() {}
