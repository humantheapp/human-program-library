#![forbid(unsafe_code)]
#![deny(clippy::all)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::result_large_err)]

pub mod error;
pub mod state;

use crate::error::FanoutError;
use crate::state::*;

use anchor_lang::{prelude::*, AnchorDeserialize, AnchorSerialize};
use std::collections::HashSet;

declare_id!("FanoutYvahiZsDeFSjDmJymY12EZ6poVH1LydbJrLTRq");

#[program]
pub mod fanout {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, members: Vec<Member>) -> Result<()> {
        if members.is_empty() || members.len() > 10 {
            return err!(FanoutError::InvalidMemberCount);
        }

        // check if shares in members add up to 10000
        let mut total_shares = 0;
        for member in members.iter() {
            if member.share == 0 || member.share > 10000 {
                return Err(FanoutError::InvalidShares.into());
            }
            total_shares += member.share as usize;
        }

        if total_shares != 10000 {
            return Err(FanoutError::InvalidShares.into());
        }

        ctx.accounts.fanout.set_inner(Fanout {
            distributed: 0,
            members,
        });
        Ok(())
    }

    pub fn distribute(ctx: Context<Distribute>) -> Result<()> {
        let fanout = &mut ctx.accounts.fanout;

        let fanout_acc = fanout.to_account_info();
        let mut fanout_lamports = fanout_acc.try_borrow_mut_lamports()?;

        // subtract the rent exemption
        let rent_minimum = Rent::get()?.minimum_balance(fanout_acc.data_len());

        let distribution_amount = fanout_lamports
            .checked_sub(rent_minimum)
            .ok_or(FanoutError::Overflow)?;

        let split = fanout
            .calculate_split(distribution_amount)
            .ok_or_else(|| error!(FanoutError::Overflow))?;

        let mut seen = HashSet::new();

        // increment lamports of each remaining account
        for acc in ctx.remaining_accounts.iter() {
            if !seen.insert(acc.key) {
                return err!(FanoutError::DuplicateMember);
            }

            let amount = *split.get(acc.key).unwrap();
            let mut acc_lamports = acc.try_borrow_mut_lamports()?;

            **acc_lamports = acc_lamports
                .checked_add(amount)
                .ok_or_else(|| error!(FanoutError::Overflow))?;

            **fanout_lamports = fanout_lamports
                .checked_sub(amount)
                .ok_or_else(|| error!(FanoutError::Overflow))?;

            fanout.distributed += amount;
        }

        // make sure no account is missing or placed more than once
        if seen.len() != fanout.members.len() {
            return err!(FanoutError::MemberNotFound);
        }

        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    /// allows account to be created at any address (e.g. via CreateAccountWithSeed)
    #[account(zero)]
    pub fanout: Account<'info, Fanout>,
}

#[derive(Accounts)]
pub struct Distribute<'info> {
    #[account(mut)]
    pub fanout: Account<'info, Fanout>,
}
