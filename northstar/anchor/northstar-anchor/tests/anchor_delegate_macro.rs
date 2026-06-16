use anchor_lang::prelude::*;
use northstar_anchor::{cpi::DelegateConfig, delegate};

declare_id!("11111111111111111111111111111111");

#[delegate]
#[derive(Accounts)]
pub struct DelegatePlayer<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: PDA checked by northstar_anchor helper from supplied seeds.
    #[account(mut, del)]
    pub player: UncheckedAccount<'info>,
}

#[allow(dead_code)]
fn uses_generated_methods(ctx: Context<DelegatePlayer>) -> Result<()> {
    let seeds: &[&[u8]] = &[b"player"];
    ctx.accounts
        .delegate_player(&ctx.accounts.payer, seeds, DelegateConfig { grid_id: 1 })?;
    ctx.accounts.undelegate_player(&ctx.accounts.payer, seeds)?;
    Ok(())
}
