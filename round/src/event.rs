use anchor_lang::prelude::*;

use crate::state::Round;

#[event]
pub struct RoundCreatedEvent {
    pub round_addr: Pubkey,
    pub round: Round,
}

#[event]
pub struct ContributeEvent {
    pub round: Pubkey,
    pub user: Pubkey,
    pub bid_mint: Pubkey,
    pub offer_mint: Pubkey,
    pub amount: u64,
}

#[event]
pub struct WithdrawEvent {
    pub round: Pubkey,
    pub user: Pubkey,
    pub bid_mint: Pubkey,
    pub offer_mint: Pubkey,
    pub amount: u64,
    pub reason: WithdrawReason,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq)]
pub enum WithdrawReason {
    UserInitiated,
    RoundRejected,
    HeirTimeout,
}

#[event]
pub struct RoundAcceptedEvent {
    pub round: Pubkey,
    pub heir: Pubkey,
    pub bid_mint: Pubkey,
    pub offer_mint: Pubkey,
    pub bid_amount: u64,
    pub offer_amount: u64,
}

#[event]
pub struct RoundRejectedEvent {
    pub round: Pubkey,
    pub heir: Pubkey,
    pub bid_mint: Pubkey,
    pub offer_mint: Pubkey,
    pub offer_amount: u64,
}

#[event]
pub struct RedeemEvent {
    pub round: Pubkey,
    pub user: Pubkey,
    pub bid_mint: Pubkey,
    pub offer_mint: Pubkey,
    pub amount: u64,
}

#[event]
pub struct RoundCancelledEvent {
    pub round: Pubkey,
    pub heir: Pubkey,
    pub bid_mint: Pubkey,
    pub offer_mint: Pubkey,
    pub bid_amount: u64,
    pub offer_amount: u64,
}

#[event]
pub struct RoundClosedEvent {
    pub round_addr: Pubkey,
    pub round: Round,
    pub returned_offer_amount: u64,
}
