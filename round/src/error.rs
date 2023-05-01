use anchor_lang::error_code;

#[error_code]
pub enum RoundError {
    #[msg("Overflow occured")]
    Overflow,
    #[msg("inteval between bidding_start and bidding_end should be positive")]
    SuspiciousOfferInterval,
    #[msg("missing delegate or delegated amount is zero (use approve token instruction)")]
    NoDelegatedAmount,
    #[msg("bidding has not yet begun (now < bidding_start)")]
    BiddingNotStarted,
    #[msg("bidding ended (now > bidding_end)")]
    BiddingEnded,
    #[msg("bidding has not yet begun (now < bidding_start)")]
    BiddingIsNotOver,
    #[msg("heir inactivity timeout has not yet passed")]
    InactivityTimeoutHasNotPassed,
    #[msg("can't withdraw bid because creator did not reject request")]
    CantWithdrawPendingOffer,
    #[msg("can't withdraw bid because creator did acept request (use claim instruction)")]
    CantWithdrawAcceptedOffer,
    #[msg("offer already accepted/rejected")]
    OfferIsNotPending,
    #[msg("time for accepting/rejecting offer has expired")]
    OfferTimedOut,
    #[msg("can't accept/reject offer while bidding has not ended")]
    BiddingStillGoing,
    #[msg("can't accept offer without contributors")]
    CantAcceptZeroOffering,
    #[msg("can't withdraw without user signature before bidding has ended")]
    CantWithdrawWithoutUserSignature,
    #[msg("can't redeem round that has not been accepted by heir")]
    CantRedeemNotAcceptedRound,
    #[msg("can't cancel round after start")]
    CantCancelStartedRound,
    #[msg("can't close round before all vouchers are withdrawn")]
    VouchersNotWithdrawn,
    #[msg("can't close before heir timeout")]
    CantCloseBeforeHeirTimeout,
}
