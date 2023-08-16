#![forbid(unsafe_code)]
#![deny(clippy::all)]
#![allow(clippy::result_large_err)]

pub mod error;
pub mod event;
pub mod state;

use std::collections::BTreeMap;

use crate::error::RoundError;
use crate::event::*;
use crate::state::*;

use anchor_lang::{
    prelude::*,
    solana_program::{clock::UnixTimestamp, program_option::COption, program_pack::Pack},
    system_program, AnchorDeserialize, AnchorSerialize,
};
use anchor_spl::{
    associated_token::get_associated_token_address,
    token::{Mint, Token, TokenAccount},
};

pub const AUTHORITY_SEED: &[u8] = b"AUTH";
pub const OFFER_SEED: &[u8] = b"OFFER";
pub const BID_SEED: &[u8] = b"BID";
pub const VOUCHER_SEED: &[u8] = b"VOUCHER";
pub const OFFCHAIN_VOUCHER_SEED: &[u8] = b"OFFCHAIN_VOUCHER"; // separe voucher namespace so they don't collide

declare_id!("Round8ieb1Jcbp4m68kwCVyUJmHAVoz4orTwU3LtAuH");

#[program]
pub mod round {
    use super::*;

    pub fn create_round(ctx: Context<CreateRound>, params: CreateRoundParams) -> Result<()> {
        // validation
        let offer_amount = ctx.accounts.offer_source_wallet.delegated_amount;
        if ctx.accounts.offer_source_wallet.delegate.is_none() || offer_amount == 0 {
            return err!(RoundError::NoDelegatedAmount);
        }

        if params.bidding_start >= params.bidding_end {
            return err!(RoundError::SuspiciousOfferInterval);
        }

        // transfer round to wallet controlled by contract
        let cpi = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            anchor_spl::token::Transfer {
                from: ctx.accounts.offer_source_wallet.to_account_info(),
                to: ctx.accounts.offer_wallet.to_account_info(),
                authority: ctx.accounts.offer_source_authority.to_account_info(),
            },
        );

        anchor_spl::token::transfer(cpi, offer_amount)?;

        let round = Round {
            status: RoundStatus::Pending,
            created_at: Clock::get()?.unix_timestamp,
            bid_mint: ctx.accounts.bid_mint.key(),
            offer_mint: ctx.accounts.offer_mint.key(),
            bidding_start: params.bidding_start,
            bidding_end: params.bidding_end,
            heir: params.heir,
            recipient: params.recipient,
            payer: *ctx.accounts.payer.key,
            vouchers_count: 0,
            total_bid: None,
            total_offer: Some(offer_amount),
            target_bid: params.target_bid,
            return_wallet: ctx.accounts.offer_source_wallet.key(),
            ..Default::default()
        };

        ctx.accounts.round.set_inner(round.clone());

        emit!(RoundCreatedEvent {
            round_addr: ctx.accounts.round.key(),
            round,
        });

        Ok(())
    }

    // Contribute bid mint to round
    pub fn contribute(ctx: Context<Contribute>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        ctx.accounts.round.assert_can_contribute(now)?;

        let Contribute {
            round,
            voucher,
            user,
            payer,
            user_wallet,
            user_wallet_authority,
            bid_wallet,
            token_program,
            ..
        } = ctx.accounts;

        let amount_to_deposit = user_wallet.delegated_amount;
        if user_wallet.delegate.is_none() || amount_to_deposit == 0 {
            return err!(RoundError::NoDelegatedAmount);
        }

        // transfer bid to wallet controlled by contract
        let cpi = CpiContext::new(
            token_program.to_account_info(),
            anchor_spl::token::Transfer {
                from: user_wallet.to_account_info(),
                to: bid_wallet.to_account_info(),
                authority: user_wallet_authority.to_account_info(),
            },
        );

        anchor_spl::token::transfer(cpi, amount_to_deposit)?;

        // add amount to user voucher (in case user contributed twice)
        voucher.amount_contributed = voucher
            .amount_contributed
            .checked_add(amount_to_deposit)
            .ok_or(error!(RoundError::Overflow))?;

        emit!(ContributeEvent {
            round: round.key(),
            user: user.key(),
            bid_mint: round.bid_mint,
            offer_mint: round.offer_mint,
            amount: amount_to_deposit,
            is_offchain: false,
        });

        if voucher.payer != Pubkey::default() {
            return Ok(());
        }

        // first time initialization things below

        // record voucher payer so it can't be changed later
        voucher.payer = payer.key();
        voucher.round = round.key();
        voucher.user = user.key();

        // keep track of vouchers
        round.vouchers_count = round
            .vouchers_count
            .checked_add(1)
            .ok_or(error!(RoundError::Overflow))?;

        Ok(())
    }

    // Withdraw user bid before round is expired
    pub fn withdraw(mut ctx: Context<Withdraw>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        ctx.accounts
            .round
            .assert_can_withdraw(now, ctx.accounts.user.is_signer)?;

        refund_bid_to_recipient(&mut ctx.accounts, &ctx.bumps)?;

        // will be erased anyways, but just to be sure
        let contributed = std::mem::take(&mut ctx.accounts.voucher.amount_contributed);
        ctx.accounts.round.vouchers_count = ctx
            .accounts
            .round
            .vouchers_count
            .checked_sub(1)
            .ok_or(error!(RoundError::Overflow))?;

        let reason = match (&ctx.accounts.round.status, ctx.accounts.user.is_signer) {
            (RoundStatus::Pending, true) => WithdrawReason::UserInitiated,
            (RoundStatus::Pending, false) => WithdrawReason::HeirTimeout,
            (RoundStatus::Rejected, _) => WithdrawReason::RoundRejected,
            _ => unreachable!("withdraw should not be called in this state"),
        };

        emit!(WithdrawEvent {
            round: ctx.accounts.round.key(),
            user: ctx.accounts.user.key(),
            bid_mint: ctx.accounts.round.bid_mint,
            offer_mint: ctx.accounts.round.offer_mint,
            amount: contributed,
            reason,
        });

        Ok(())
    }

    pub fn accept(
        mut ctx: Context<Accept>,
        reconciliation_authority: Option<Pubkey>,
    ) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        ctx.accounts.round.assert_can_accept_or_reject(now)?;

        ctx.accounts.round.total_bid = Some(ctx.accounts.bid_wallet.amount);
        ctx.accounts.round.total_offer = Some(ctx.accounts.offer_wallet.amount);

        transfer_bid_to_recipient(&mut ctx)?;

        if let Some(auth) = reconciliation_authority {
            msg!("reconciliation authority: {}", auth);
            ctx.accounts.round.status = RoundStatus::Reconciliation;
            ctx.accounts.round.reconciliation_authority = auth;
            return Ok(());
        }

        ctx.accounts.round.status = RoundStatus::Accepted;

        emit!(RoundAcceptedEvent {
            round: ctx.accounts.round.key(),
            heir: ctx.accounts.heir.key(),
            bid_mint: ctx.accounts.round.bid_mint,
            offer_mint: ctx.accounts.round.offer_mint,
            bid_amount: ctx.accounts.round.total_bid.unwrap(),
            offer_amount: ctx.accounts.round.total_offer.unwrap(),
        });

        Ok(())
    }

    // Contribute bid mint to round
    pub fn record_offchain_contribution(
        ctx: Context<RecordOffchainContribution>,
        amount_lamports: u64,
    ) -> Result<()> {
        ctx.accounts.round.assert_can_contribute_offchain()?;

        let RecordOffchainContribution {
            round,
            voucher,
            user,
            payer,
            ..
        } = ctx.accounts;

        emit!(ContributeEvent {
            round: round.key(),
            user: user.key(),
            bid_mint: round.bid_mint,
            offer_mint: round.offer_mint,
            amount: amount_lamports,
            is_offchain: true,
        });

        if voucher.payer != Pubkey::default() {
            msg!("contribute_offchain can be used only once per user");
            return Ok(());
        }

        voucher.set_inner(Voucher {
            user: user.key(),
            round: round.key(),
            payer: payer.key(),
            amount_contributed: amount_lamports,
            is_fiat: true,
            reserved1: [0; 31],
            reserved2: Pubkey::default(),
            reserved3: Pubkey::default(),
        });

        // keep track of vouchers
        round.vouchers_count = round
            .vouchers_count
            .checked_add(1)
            .ok_or(error!(RoundError::Overflow))?;

        Ok(())
    }

    pub fn finish_reconciliation(ctx: Context<FinishReconciliation>) -> Result<()> {
        ctx.accounts.round.assert_can_finish_reconciliation()?;

        ctx.accounts.round.status = RoundStatus::Accepted;

        emit!(RoundAcceptedEvent {
            round: ctx.accounts.round.key(),
            heir: ctx.accounts.round.heir,
            bid_mint: ctx.accounts.round.bid_mint,
            offer_mint: ctx.accounts.round.offer_mint,
            bid_amount: ctx.accounts.round.total_bid.unwrap(),
            offer_amount: ctx.accounts.round.total_offer.unwrap(),
        });

        Ok(())
    }

    pub fn reject(mut ctx: Context<Reject>) -> Result<()> {
        let Reject { round, .. } = ctx.accounts;

        let now = Clock::get()?.unix_timestamp;
        round.assert_can_accept_or_reject(now)?;

        round.status = RoundStatus::Rejected;

        transfer_offer_back_to_heir(&mut ctx)?;

        emit!(RoundRejectedEvent {
            round: ctx.accounts.round.key(),
            heir: ctx.accounts.heir.key(),
            bid_mint: ctx.accounts.round.bid_mint,
            offer_mint: ctx.accounts.round.offer_mint,
            offer_amount: ctx.accounts.offer_wallet.amount,
        });

        Ok(())
    }

    pub fn reject_bid(ctx: Context<RejectBid>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        ctx.accounts.round.assert_can_reject_bid(now)?;

        let mut accounts = ctx.accounts.to_withdraw_ctx();
        refund_bid_to_recipient(&mut accounts, &ctx.bumps)?;

        // will be erased anyways, but to be sure
        ctx.accounts.voucher.amount_contributed = 0;
        ctx.accounts.round.vouchers_count = ctx
            .accounts
            .round
            .vouchers_count
            .checked_sub(1)
            .ok_or(error!(RoundError::Overflow))?;

        emit!(BidRejectedEvent {
            round: ctx.accounts.round.key(),
            user: ctx.accounts.user.key(),
            bid_mint: ctx.accounts.round.bid_mint,
            offer_mint: ctx.accounts.round.offer_mint,
            amount: ctx.accounts.bid_wallet.amount,
        });

        Ok(())
    }

    // SIR DO NOT REDEEM
    pub fn redeem(mut ctx: Context<Redeem>) -> Result<()> {
        ctx.accounts.round.assert_can_redeem()?;

        // remove in next update when target_bid can't be zero anymore
        let target = if ctx.accounts.round.target_bid != 0 {
            ctx.accounts.round.target_bid
        } else {
            ctx.accounts.round.total_bid.unwrap()
        };

        let amount = calculate_redeem_amount(
            target,
            ctx.accounts.round.total_bid.unwrap(),
            ctx.accounts.voucher.amount_contributed,
            ctx.accounts.round.total_offer.unwrap(),
        )
        .ok_or(error!(RoundError::Overflow))?;

        ctx.accounts.voucher.amount_contributed = 0;
        ctx.accounts.round.vouchers_count = ctx
            .accounts
            .round
            .vouchers_count
            .checked_sub(1)
            .ok_or(error!(RoundError::Overflow))?;

        transfer_to_user(&mut ctx, amount)?;

        emit!(RedeemEvent {
            round: ctx.accounts.round.key(),
            user: ctx.accounts.user.key(),
            bid_mint: ctx.accounts.round.bid_mint,
            offer_mint: ctx.accounts.round.offer_mint,
            amount,
        });

        Ok(())
    }

    // cancel round before it begins
    pub fn cancel(ctx: Context<Cancel>) -> Result<()> {
        let Cancel {
            round,
            offer_wallet,
            return_wallet,
            bid_wallet,
            token_program,
            authority,
            payer,
            heir,
            ..
        } = ctx.accounts;

        let now = Clock::get()?.unix_timestamp;
        round.assert_can_cancel(now)?;

        assert_eq!(round.vouchers_count, 0, "impossible voucher count");

        let round_key = round.key();
        let authority_seeds: &[&[&[u8]]] = &[&[
            AUTHORITY_SEED,
            round_key.as_ref(),
            &[*ctx.bumps.get("authority").unwrap()],
        ]];

        // transfer offer back
        let cpi = CpiContext::new_with_signer(
            token_program.to_account_info(),
            anchor_spl::token::Transfer {
                from: offer_wallet.to_account_info(),
                to: return_wallet.to_account_info(),
                authority: authority.to_account_info(),
            },
            authority_seeds,
        );
        anchor_spl::token::transfer(cpi, offer_wallet.amount)?;

        // close offer
        let cpi = CpiContext::new_with_signer(
            token_program.to_account_info(),
            anchor_spl::token::CloseAccount {
                account: offer_wallet.to_account_info(),
                destination: payer.to_account_info(),
                authority: authority.to_account_info(),
            },
            authority_seeds,
        );
        anchor_spl::token::close_account(cpi)?;

        let cpi = CpiContext::new_with_signer(
            token_program.to_account_info(),
            anchor_spl::token::CloseAccount {
                account: bid_wallet.to_account_info(),
                destination: payer.to_account_info(),
                authority: authority.to_account_info(),
            },
            authority_seeds,
        );
        anchor_spl::token::close_account(cpi)?;

        emit!(RoundCancelledEvent {
            round: round.key(),
            heir: heir.key(),
            bid_mint: round.bid_mint,
            offer_mint: round.offer_mint,
            bid_amount: bid_wallet.amount,
            offer_amount: offer_wallet.amount,
        });

        Ok(())
    }

    pub fn close(ctx: Context<Close>) -> Result<()> {
        // check if round can be cleaned up
        let Close {
            round,
            return_wallet,
            offer_wallet,
            bid_wallet,
            token_program,
            authority,
            payer,
            ..
        } = ctx.accounts;

        let now = Clock::get()?.unix_timestamp;
        round.assert_can_close(now)?;

        let round_key = round.key();
        let authority_seeds: &[&[&[u8]]] = &[&[
            AUTHORITY_SEED,
            round_key.as_ref(),
            &[*ctx.bumps.get("authority").unwrap()],
        ]];

        // transfer offer back
        let cpi = CpiContext::new_with_signer(
            token_program.to_account_info(),
            anchor_spl::token::Transfer {
                from: offer_wallet.to_account_info(),
                to: return_wallet.to_account_info(),
                authority: authority.to_account_info(),
            },
            authority_seeds,
        );
        anchor_spl::token::transfer(cpi, offer_wallet.amount)?;

        // close offer
        let cpi = CpiContext::new_with_signer(
            token_program.to_account_info(),
            anchor_spl::token::CloseAccount {
                account: offer_wallet.to_account_info(),
                destination: payer.to_account_info(),
                authority: authority.to_account_info(),
            },
            authority_seeds,
        );
        anchor_spl::token::close_account(cpi)?;

        // close bid if it exists
        if *bid_wallet.owner == spl_token::ID {
            let cpi = CpiContext::new_with_signer(
                token_program.to_account_info(),
                anchor_spl::token::CloseAccount {
                    account: bid_wallet.to_account_info(),
                    destination: payer.to_account_info(),
                    authority: authority.to_account_info(),
                },
                authority_seeds,
            );
            anchor_spl::token::close_account(cpi)?;
        }

        emit!(RoundClosedEvent {
            round_addr: round.key(),
            round: Clone::clone(round),
            returned_offer_amount: offer_wallet.amount,
        });

        Ok(())
    }

    pub fn migrate(ctx: Context<Migrate>) -> Result<()> {
        let round = &mut ctx.accounts.round;
        if round.return_wallet == Pubkey::default() {
            let return_wallet = get_associated_token_address(&round.heir, &round.offer_mint);

            round.return_wallet = return_wallet;
        }

        Ok(())
    }
}

fn refund_bid_to_recipient(accounts: &mut Withdraw, bumps: &BTreeMap<String, u8>) -> Result<()> {
    let Withdraw {
        round,
        voucher,
        user,
        user_wallet,
        bid_wallet,
        authority,
        wsol_mint,
        payer: _payer,
        current_payer,
        token_program,
        system_program,
    } = accounts;

    let round_key = round.key();
    let authority_seeds: &[&[&[u8]]] = &[&[
        AUTHORITY_SEED,
        round_key.as_ref(),
        &[*bumps.get("authority").unwrap()],
    ]];

    if !bid_wallet.is_native() {
        // if this is token (e.g. usdc), just transfer from bid to user wallet
        let cpi = CpiContext::new_with_signer(
            token_program.to_account_info(),
            anchor_spl::token::Transfer {
                from: bid_wallet.to_account_info(),
                to: user_wallet.to_account_info(),
                authority: authority.to_account_info(),
            },
            authority_seeds,
        );
        anchor_spl::token::transfer(cpi, voucher.amount_contributed)?;
        return Ok(());
    }

    // if wsol, transfer from bid wallet to temp addr, unwrap to another temp addr, transfer directly to user account/payer
    let space = spl_token::state::Account::LEN;
    let rent = Rent::get()?.minimum_balance(space);

    // init temp wallet
    {
        let cpi = CpiContext::new_with_signer(
            system_program.to_account_info(),
            system_program::CreateAccount {
                from: current_payer.to_account_info(),
                to: authority.to_account_info(),
            },
            authority_seeds,
        );
        system_program::create_account(cpi, rent, space as u64, &spl_token::ID)?;

        let cpi = CpiContext::new(
            token_program.to_account_info(),
            anchor_spl::token::InitializeAccount3 {
                account: authority.to_account_info(),
                mint: wsol_mint.to_account_info(),
                authority: authority.to_account_info(),
            },
        );
        anchor_spl::token::initialize_account3(cpi)?;
    }

    // transfer from bid wallet to authority wallet
    let cpi = CpiContext::new_with_signer(
        token_program.to_account_info(),
        anchor_spl::token::Transfer {
            from: bid_wallet.to_account_info(),
            to: authority.to_account_info(),
            authority: authority.to_account_info(),
        },
        authority_seeds,
    );
    anchor_spl::token::transfer(cpi, voucher.amount_contributed)?;

    // close account to current_payer
    let cpi = CpiContext::new_with_signer(
        token_program.to_account_info(),
        anchor_spl::token::CloseAccount {
            account: authority.to_account_info(),
            destination: current_payer.to_account_info(),
            authority: authority.to_account_info(),
        },
        authority_seeds,
    );
    anchor_spl::token::close_account(cpi)?;

    // payer should pay back to user
    let cpi = CpiContext::new_with_signer(
        system_program.to_account_info(),
        system_program::Transfer {
            from: current_payer.to_account_info(),
            to: user.to_account_info(),
        },
        authority_seeds,
    );
    system_program::transfer(cpi, voucher.amount_contributed)?;

    Ok(())
}

fn transfer_bid_to_recipient(ctx: &mut Context<Accept>) -> Result<()> {
    let Accept {
        round,
        bid_wallet,
        recipient,
        token_program,
        authority,
        payer,
        ..
    } = ctx.accounts;

    let round_key = round.key();
    let authority_seeds: &[&[&[u8]]] = &[&[
        AUTHORITY_SEED,
        round_key.as_ref(),
        &[*ctx.bumps.get("authority").unwrap()],
    ]];

    if let COption::Some(rent) = bid_wallet.is_native {
        // unwrap SOL's and send them directly
        // this way recipient can be simple SOL address
        // or a wrapped SOL wallet

        let balance = bid_wallet.amount;

        let cpi = CpiContext::new_with_signer(
            token_program.to_account_info(),
            anchor_spl::token::CloseAccount {
                account: bid_wallet.to_account_info(),
                destination: authority.to_account_info(),
                authority: authority.to_account_info(),
            },
            authority_seeds,
        );
        anchor_spl::token::close_account(cpi)?;

        let cpi = CpiContext::new_with_signer(
            token_program.to_account_info(),
            system_program::Transfer {
                from: authority.to_account_info(),
                to: recipient.to_account_info(),
            },
            authority_seeds,
        );
        system_program::transfer(cpi, balance)?;

        // refund rent
        let cpi = CpiContext::new_with_signer(
            token_program.to_account_info(),
            system_program::Transfer {
                from: authority.to_account_info(),
                to: payer.to_account_info(),
            },
            authority_seeds,
        );
        system_program::transfer(cpi, rent)?;

        return Ok(());
    }

    // or just transfer bid to spl wallet
    let cpi = CpiContext::new_with_signer(
        token_program.to_account_info(),
        anchor_spl::token::Transfer {
            from: bid_wallet.to_account_info(),
            to: recipient.to_account_info(),
            authority: authority.to_account_info(),
        },
        authority_seeds,
    );

    let amount = bid_wallet.amount; // transfer all round back
    anchor_spl::token::transfer(cpi, amount)?;
    Ok(())
}

fn transfer_offer_back_to_heir(ctx: &mut Context<Reject>) -> Result<()> {
    let Reject {
        round,
        offer_wallet,
        return_wallet,
        token_program,
        authority,
        ..
    } = ctx.accounts;

    let round_key = round.key();
    let authority_seeds: &[&[&[u8]]] = &[&[
        AUTHORITY_SEED,
        round_key.as_ref(),
        &[*ctx.bumps.get("authority").unwrap()],
    ]];

    // transfer bid to wallet controlled by contract
    let cpi = CpiContext::new_with_signer(
        token_program.to_account_info(),
        anchor_spl::token::Transfer {
            from: offer_wallet.to_account_info(),
            to: return_wallet.to_account_info(),
            authority: authority.to_account_info(),
        },
        authority_seeds,
    );

    let amount = offer_wallet.amount; // transfer all round back
    anchor_spl::token::transfer(cpi, amount)?;

    Ok(())
}

fn transfer_to_user(ctx: &mut Context<Redeem>, amount: u64) -> Result<()> {
    let Redeem {
        round,
        offer_wallet,
        user_wallet,
        token_program,
        authority,
        ..
    } = ctx.accounts;

    let round_key = round.key();
    let authority_seeds: &[&[&[u8]]] = &[&[
        AUTHORITY_SEED,
        round_key.as_ref(),
        &[*ctx.bumps.get("authority").unwrap()],
    ]];

    // transfer bid to wallet controlled by contract
    let cpi = CpiContext::new_with_signer(
        token_program.to_account_info(),
        anchor_spl::token::Transfer {
            from: offer_wallet.to_account_info(),
            to: user_wallet.to_account_info(),
            authority: authority.to_account_info(),
        },
        authority_seeds,
    );

    anchor_spl::token::transfer(cpi, amount)?;

    Ok(())
}

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct CreateRoundParams {
    /// has ability to accept/reject bid
    pub heir: Pubkey,
    /// bid recipient
    pub recipient: Pubkey,
    /// used to dillute
    pub target_bid: u64,

    pub bidding_start: UnixTimestamp,
    pub bidding_end: UnixTimestamp,
}

#[derive(Accounts)]
pub struct CreateRound<'info> {
    #[account(
        init,
        space = 8 + Round::INIT_SPACE,
        payer = payer,
    )]
    pub round: Box<Account<'info, Round>>,

    #[account(
        init,
        payer = payer,
        token::authority = authority,
        token::mint = offer_mint,
        seeds = [OFFER_SEED, round.key().as_ref()],
        bump,
    )]
    pub offer_wallet: Account<'info, TokenAccount>,
    pub offer_mint: Account<'info, Mint>,

    #[account(mut)]
    pub offer_source_wallet: Account<'info, TokenAccount>,
    pub offer_source_authority: Signer<'info>,

    #[account(
        init,
        payer = payer,
        token::authority = authority,
        token::mint = bid_mint,
        seeds = [BID_SEED, round.key().as_ref()],
        bump,
    )]
    pub bid_wallet: Account<'info, TokenAccount>,
    pub bid_mint: Account<'info, Mint>,

    /// CHECK: seeds are checked
    #[account(
        seeds = [AUTHORITY_SEED, round.key().as_ref()],
        bump,
    )]
    pub authority: UncheckedAccount<'info>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Contribute<'info> {
    #[account(mut)]
    pub round: Box<Account<'info, Round>>,

    #[account(
        init_if_needed,
        space = Voucher::calculate_space(),
        payer = payer,
        seeds = [VOUCHER_SEED, round.key().as_ref(), user.key.as_ref()],
        bump,
    )]
    pub voucher: Account<'info, Voucher>,
    pub user: Signer<'info>,

    #[account(mut)]
    pub user_wallet: Account<'info, TokenAccount>,
    pub user_wallet_authority: Signer<'info>,

    #[account(
        mut,
        seeds = [BID_SEED, round.key().as_ref()],
        bump,
    )]
    pub bid_wallet: Account<'info, TokenAccount>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RecordOffchainContribution<'info> {
    #[account(
        mut,
        has_one = reconciliation_authority,
    )]
    pub round: Box<Account<'info, Round>>,

    #[account(
        init_if_needed,
        space = Voucher::calculate_space(),
        payer = payer,
        seeds = [OFFCHAIN_VOUCHER_SEED, round.key().as_ref(), user.key.as_ref()],
        bump,
    )]
    pub voucher: Account<'info, Voucher>,

    /// CHECK: user is unused
    pub user: UncheckedAccount<'info>,

    pub reconciliation_authority: Signer<'info>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Withdraw<'info> {
    #[account(mut)]
    pub round: Box<Account<'info, Round>>,

    #[account(
        mut,
        seeds = [VOUCHER_SEED, round.key().as_ref(), user.key().as_ref()],
        bump,
        has_one = user,
        close = payer,
        has_one = payer,
    )]
    pub voucher: Account<'info, Voucher>,

    /// CHECK: Somethimes withdraw call is permissionless, so signature is checked in code
    #[account(mut)]
    pub user: UncheckedAccount<'info>,

    /// CHECK: seeds are checked, we don't care about account being initialized
    #[account(
        mut,
        constraint = user_wallet.key() == get_associated_token_address(
            user.key,
            &bid_wallet.mint
        ),
    )]
    pub user_wallet: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [BID_SEED, round.key().as_ref()],
        bump,
    )]
    pub bid_wallet: Account<'info, TokenAccount>,

    /// CHECK: seeds are checked
    #[account(
        mut,
        seeds = [AUTHORITY_SEED, round.key().as_ref()],
        bump,
    )]
    pub authority: UncheckedAccount<'info>,

    /// WSOL mint
    #[account(constraint = wsol_mint.key() == spl_token::native_mint::ID)]
    pub wsol_mint: Account<'info, Mint>,

    /// CHECK: We just transfer SOL from closing voucher to this account
    /// payer should equal to payer field on voucher
    /// so user can't just steal rent someone else maybe paid
    #[account(mut)]
    pub payer: UncheckedAccount<'info>,

    /// current payer should have some SOL to temporarily open WSOL account
    /// these funds are returned at the end of the call
    #[account(mut)]
    pub current_payer: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RejectBid<'info> {
    #[account(
        mut,
        has_one = heir,
    )]
    pub round: Box<Account<'info, Round>>,

    #[account(
        mut,
        seeds = [VOUCHER_SEED, round.key().as_ref(), user.key().as_ref()],
        bump,
        has_one = user,
        has_one = payer,
        close = payer,
    )]
    pub voucher: Account<'info, Voucher>,

    pub heir: Signer<'info>,

    /// CHECK: no signature required
    #[account(mut)]
    pub user: UncheckedAccount<'info>,

    /// CHECK: seeds are checked, we don't care about account being initialized
    #[account(
        mut,
        constraint = user_wallet.key() == get_associated_token_address(
            user.key,
            &bid_wallet.mint
        ),
    )]
    pub user_wallet: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [BID_SEED, round.key().as_ref()],
        bump,
    )]
    pub bid_wallet: Account<'info, TokenAccount>,

    /// CHECK: seeds are checked
    #[account(
        mut,
        seeds = [AUTHORITY_SEED, round.key().as_ref()],
        bump,
    )]
    pub authority: UncheckedAccount<'info>,

    /// WSOL mint
    #[account(constraint = wsol_mint.key() == spl_token::native_mint::ID)]
    pub wsol_mint: Account<'info, Mint>,

    /// CHECK: We just transfer SOL from closing voucher to this account
    /// payer should equal to payer field on voucher
    /// so user can't just steal rent someone else maybe paid
    #[account(mut)]
    pub payer: UncheckedAccount<'info>,

    /// current payer should have some SOL to temporarily open WSOL account
    /// these funds are returned at the end of the call
    #[account(mut)]
    pub current_payer: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

impl<'a> RejectBid<'a> {
    fn to_withdraw_ctx(&self) -> Withdraw<'a> {
        Withdraw {
            round: self.round.clone(),
            voucher: self.voucher.clone(),
            user: self.user.clone(),
            user_wallet: self.user_wallet.clone(),
            bid_wallet: self.bid_wallet.clone(),
            authority: self.authority.clone(),
            wsol_mint: self.wsol_mint.clone(),
            payer: self.payer.clone(),
            current_payer: self.current_payer.clone(),
            token_program: self.token_program.clone(),
            system_program: self.system_program.clone(),
        }
    }
}

#[derive(Accounts)]
pub struct Accept<'info> {
    #[account(mut, has_one = heir, has_one = recipient)]
    pub round: Box<Account<'info, Round>>,

    pub heir: Signer<'info>,

    #[account(
        mut,
        seeds = [OFFER_SEED, round.key().as_ref()],
        bump,
    )]
    pub offer_wallet: Account<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [BID_SEED, round.key().as_ref()],
        bump,
    )]
    pub bid_wallet: Account<'info, TokenAccount>,

    /// CHECK: this could be SOL address or token account, but the address is checked
    #[account(mut)]
    pub recipient: UncheckedAccount<'info>,

    /// CHECK: seeds are checked
    #[account(
        seeds = [AUTHORITY_SEED, round.key().as_ref()],
        bump,
        mut
    )]
    pub authority: UncheckedAccount<'info>,

    /// CHECK: we just refund bid_wallet rent to this account
    #[account(mut)]
    pub payer: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Reject<'info> {
    #[account(mut, has_one = heir, has_one = payer, has_one = return_wallet)]
    pub round: Box<Account<'info, Round>>,

    pub heir: Signer<'info>,

    #[account(
        mut,
        seeds = [OFFER_SEED, round.key().as_ref()],
        bump,
    )]
    pub offer_wallet: Account<'info, TokenAccount>,

    #[account(mut)]
    pub return_wallet: Account<'info, TokenAccount>,

    /// CHECK: seeds are checked
    #[account(
        seeds = [AUTHORITY_SEED, round.key().as_ref()],
        bump,
        mut,
    )]
    pub authority: UncheckedAccount<'info>,

    /// CHECK: we just transfer SOL to this account if round has zero vouchers
    #[account(mut)]
    pub payer: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Redeem<'info> {
    #[account(mut)]
    pub round: Box<Account<'info, Round>>,

    #[account(
        mut,
        seeds = [VOUCHER_SEED, round.key().as_ref(), user.key.as_ref()],
        bump,
        close = payer,
        has_one = payer,
    )]
    pub voucher: Account<'info, Voucher>,
    pub user: Signer<'info>,

    #[account(mut)]
    pub user_wallet: Account<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [OFFER_SEED, round.key().as_ref()],
        bump,
    )]
    pub offer_wallet: Account<'info, TokenAccount>,

    /// CHECK: seeds are checked
    #[account(
        seeds = [AUTHORITY_SEED, round.key().as_ref()],
        bump,
    )]
    pub authority: UncheckedAccount<'info>,

    /// CHECK: we just transfer SOL to this account bc we close voucher
    #[account(mut)]
    pub payer: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Cancel<'info> {
    #[account(
        mut,
        has_one = heir,
        has_one = payer,
        has_one = return_wallet,
        close = payer
    )]
    pub round: Box<Account<'info, Round>>,

    pub heir: Signer<'info>,

    // where to refund offer to
    #[account(mut)]
    pub return_wallet: Account<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [OFFER_SEED, round.key().as_ref()],
        bump,
    )]
    pub offer_wallet: Account<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [BID_SEED, round.key().as_ref()],
        bump,
    )]
    pub bid_wallet: Account<'info, TokenAccount>,

    /// CHECK: seeds are checked
    #[account(
        seeds = [AUTHORITY_SEED, round.key().as_ref()],
        bump,
    )]
    pub authority: UncheckedAccount<'info>,

    /// CHECK: we just transfer SOL to this account in case balance becomes zero
    #[account(mut)]
    pub payer: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Close<'info> {
    #[account(
        mut,
        has_one = payer,
        has_one = return_wallet,
        close = payer
    )]
    pub round: Box<Account<'info, Round>>,

    // where to refund offer to
    #[account(mut)]
    pub return_wallet: Account<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [OFFER_SEED, round.key().as_ref()],
        bump,
    )]
    pub offer_wallet: Account<'info, TokenAccount>,

    /// CHECK: could be closed
    #[account(
        mut,
        seeds = [BID_SEED, round.key().as_ref()],
        bump,
    )]
    pub bid_wallet: UncheckedAccount<'info>,

    /// CHECK: seeds are checked
    #[account(
        seeds = [AUTHORITY_SEED, round.key().as_ref()],
        bump,
    )]
    pub authority: UncheckedAccount<'info>,

    /// CHECK: we just transfer SOL to this account in case balance becomes zero
    #[account(mut)]
    pub payer: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct FinishReconciliation<'info> {
    #[account(
        mut,
         has_one = reconciliation_authority,
        )]
    pub round: Box<Account<'info, Round>>,

    pub reconciliation_authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct Migrate<'info> {
    #[account(mut)]
    pub round: Box<Account<'info, Round>>,
}
