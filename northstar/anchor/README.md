# Northstar Anchor helpers

Separate Cargo workspace for Anchor-facing helpers. This is intentionally not a member of the Agave/Northstar workspace in `../../Cargo.toml`.

## PDA delegation macro

```rust
use anchor_lang::prelude::*;
use northstar_anchor::{delegate, cpi::DelegateConfig};

#[delegate]
#[derive(Accounts)]
pub struct DelegatePlayer<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: PDA verified by seeds passed to `delegate_player`.
    #[account(mut, del)]
    pub player: UncheckedAccount<'info>,
}

pub fn delegate_player(ctx: Context<DelegatePlayer>, grid_id: u64) -> Result<()> {
    let seeds: &[&[u8]] = &[b"player", ctx.accounts.payer.key.as_ref()];
    ctx.accounts.delegate_player(
        &ctx.accounts.payer,
        seeds,
        DelegateConfig { grid_id },
    )?;
    Ok(())
}
```

For every `#[account(mut, del)]` field, `#[delegate]` injects:

- `buffer_<field>`: owner-program PDA used to stage account bytes.
- `delegation_record_<field>`: Portal delegation record PDA.
- `owner_program`: constrained to `crate::id()` if not supplied.
- `portal_program`: Northstar Portal program account if not supplied.
- `session`: Portal session PDA if not supplied.
- `system_program`: System program if not supplied.

It also generates:

- `delegate_<field>(payer, seeds, DelegateConfig)`
- `undelegate_<field>(authority, seeds)`

`delegate_<field>` copies PDA data into the buffer, zeros the PDA, assigns the PDA to Portal via `invoke_signed`, then CPIs into Portal `Delegate`.

`undelegate_<field>` copies Portal-owned PDA data into the buffer, CPIs into Portal `UndelegateHandoff`, restores data after ownership returns to the owner program, then closes the buffer.
