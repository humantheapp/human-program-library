use crate::error::RoundError;
use anchor_lang::{prelude::*, solana_program::clock::UnixTimestamp};
use spl_math::precise_number::PreciseNumber;

#[account]
#[derive(Default)]
pub struct Round {
    /// current offer status
    pub status: RoundStatus,

    /// users offering token (typically wSOL)
    /// this is embedded in the account to avoid extra lookups
    /// and emit it in events
    pub bid_mint: Pubkey,

    /// creator offering token
    pub offer_mint: Pubkey,

    /// account that can accept/reject bid
    pub heir: Pubkey,

    /// account that receives bid in case of accept
    pub recipient: Pubkey,

    /// account to refund SOL from closing this account
    pub payer: Pubkey,

    /// number of vouchers issued (we can't close account until everybody withdraws)
    pub vouchers_count: u64,

    /// the moment round was created
    pub created_at: i64,

    pub bidding_start: i64,
    pub bidding_end: i64,

    /// total bid balance when drop was accepted. Used for calculations
    pub total_bid: Option<u64>,

    /// total offer balance when drop was accepted. Used for calculations
    pub total_offer: Option<u64>,

    // amount of token that round wants to achieve
    pub target_bid: u64,

    // wallet to return offer when round is rejected/cancelled
    pub return_wallet: Pubkey,

    pub reconciliation_authority: Pubkey,

    pub reserved1: [u8; 24],
    pub reserved2: Pubkey,
}

const _: [(); Round::INIT_SPACE] = [(); 339];

#[derive(Debug, Clone, PartialEq, AnchorSerialize, AnchorDeserialize)]
pub enum RoundStatus {
    /// Available to deposit and withdraw until bidding has ended
    Pending,
    /// Users can redeem
    Accepted,
    /// Users can withdraw
    Rejected,
    /// Users are unable to deposit or withdraw, but admin has record regarding fiat contributions
    Reconciliation,
}

impl Default for RoundStatus {
    fn default() -> Self {
        Self::Pending
    }
}

const WITHDRAWAL_ENABLED_AFTER_INACTIVITY_TIMEOUT: i64 = 7 * 24 * 60 * 60; // 7 Days

impl Round {
    pub const INIT_SPACE: usize = 339;

    pub fn assert_can_accept_or_reject(&self, now: UnixTimestamp) -> Result<()> {
        if self.status != RoundStatus::Pending {
            return err!(RoundError::OfferIsNotPending);
        }

        if self.can_withdraw_due_heir_inactivity(now)? {
            return err!(RoundError::OfferTimedOut);
        }

        if now < self.bidding_end {
            return err!(RoundError::BiddingStillGoing);
        }

        Ok(())
    }

    pub fn assert_can_contribute(&self, now: UnixTimestamp) -> Result<()> {
        if now < self.bidding_start {
            return err!(RoundError::BiddingNotStarted);
        }

        if self.bidding_end < now {
            return err!(RoundError::BiddingEnded);
        }

        // ensures we are between start and end
        Ok(())
    }

    pub fn assert_can_contribute_offchain(&self) -> Result<()> {
        if self.status != RoundStatus::Reconciliation {
            return err!(RoundError::CantContributeFiatNotInReconciliation);
        }
        Ok(())
    }

    pub fn assert_can_finish_reconciliation(&self) -> Result<()> {
        if self.status != RoundStatus::Reconciliation {
            return err!(RoundError::CantContributeFiatNotInReconciliation);
        }
        Ok(())
    }

    pub fn assert_can_withdraw(&self, now: UnixTimestamp, user_is_signer: bool) -> Result<()> {
        if now < self.bidding_start {
            return err!(RoundError::BiddingNotStarted);
        }

        if self.status == RoundStatus::Reconciliation {
            return err!(RoundError::CantWithdrawAcceptedOffer);
        }

        if self.status == RoundStatus::Accepted {
            return err!(RoundError::CantWithdrawDuringReconciliation);
        }

        if self.status == RoundStatus::Pending
            && now > self.bidding_end
            && !self.can_withdraw_due_heir_inactivity(now)?
        {
            return err!(RoundError::InactivityTimeoutHasNotPassed);
        }

        if !user_is_signer && now < self.bidding_end {
            return err!(RoundError::CantWithdrawWithoutUserSignature);
        }

        Ok(())
    }

    pub fn assert_can_reject_bid(&self, now: UnixTimestamp) -> Result<()> {
        if self.status != RoundStatus::Pending {
            return err!(RoundError::OfferIsNotPending);
        }

        if now < self.bidding_end {
            return err!(RoundError::BiddingStillGoing);
        }

        if now > self.heir_timeout_date()? {
            return err!(RoundError::OfferTimedOut);
        }

        Ok(())
    }

    fn heir_timeout_date(&self) -> Result<UnixTimestamp> {
        self.bidding_end
            .checked_add(WITHDRAWAL_ENABLED_AFTER_INACTIVITY_TIMEOUT)
            .ok_or_else(|| error!(RoundError::Overflow))
    }

    fn can_withdraw_due_heir_inactivity(&self, now: UnixTimestamp) -> Result<bool> {
        Ok(now > self.heir_timeout_date()?)
    }

    pub fn assert_can_redeem(&self) -> Result<()> {
        if self.status != RoundStatus::Accepted {
            return err!(RoundError::CantRedeemNotAcceptedRound);
        }
        Ok(())
    }

    pub fn assert_can_cancel(&self, now: UnixTimestamp) -> Result<()> {
        if now > self.bidding_start {
            return err!(RoundError::CantCancelStartedRound);
        }

        Ok(())
    }

    pub fn assert_can_close(&self, now: UnixTimestamp) -> Result<()> {
        if self.vouchers_count > 0 {
            return err!(RoundError::VouchersNotWithdrawn);
        }

        match self.status {
            RoundStatus::Accepted | RoundStatus::Rejected => Ok(()),
            RoundStatus::Pending if now > self.heir_timeout_date()? => Ok(()),
            RoundStatus::Pending => err!(RoundError::CantCloseBeforeHeirTimeout),
            RoundStatus::Reconciliation => err!(RoundError::CantCloseDuringReconciliation),
        }
    }
}

pub fn calculate_redeem_amount(
    target_bid: u64,
    bid_balance: u64,
    user_bid: u64,
    total_offer: u64,
) -> Option<u64> {
    let bid = target_bid.max(bid_balance);

    let target_bid = PreciseNumber::new(bid as u128)?;
    let user_bid = PreciseNumber::new(user_bid as u128)?;
    let total = PreciseNumber::new(total_offer as u128)?;

    user_bid
        .checked_div(&target_bid)?
        .checked_mul(&total)?
        .to_imprecise()?
        .try_into()
        .ok()
}

#[account]
pub struct Voucher {
    /// for reverse lookup
    pub user: Pubkey,
    /// for reverse lookup
    pub round: Pubkey,
    /// account that paid rent for this voucher
    pub payer: Pubkey,
    /// amount user deposited in this offer
    pub amount_contributed: u64,
    // whether actual money from this contribution was sourced off-chain
    pub is_fiat: bool,
    // reserved
    pub reserved1: [u8; 31],
    pub reserved2: Pubkey,
    pub reserved3: Pubkey,
}

impl Voucher {
    pub fn calculate_space() -> usize {
        8 + 32 + 32 + 32 + 8 + 32 + 32 + 32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn test_calculate_redeem_amount() {
        // table test
        struct TestCase {
            target_bid: u64,
            bid_balance: u64,
            user_bid: u64,
            total_offer: u64,
            expected: u64,
        }

        let cases = vec![
            TestCase {
                bid_balance: 100,
                target_bid: 100,
                user_bid: 10,
                total_offer: 100,
                expected: 10,
            },
            TestCase {
                bid_balance: 10000,
                target_bid: 1000,
                user_bid: 10000,
                total_offer: 100,
                expected: 100,
            },
            // matches target
            TestCase {
                bid_balance: 10000,
                target_bid: 10000,
                user_bid: 9000,
                total_offer: 1000,
                expected: 900,
            },
            // below target
            TestCase {
                bid_balance: 10000,
                target_bid: 26_250,
                user_bid: 9000,
                total_offer: 2500,
                expected: 857,
            },
            // above target
            TestCase {
                bid_balance: 25_000,
                target_bid: 5_500,
                user_bid: 899,
                total_offer: 500,
                expected: 18,
            },
        ];

        for case in cases {
            let result = calculate_redeem_amount(
                case.target_bid,
                case.bid_balance,
                case.user_bid,
                case.total_offer,
            );
            assert_eq!(result, Some(case.expected));
        }
    }

    #[test]
    fn test_can_contribute() {
        let round = Round {
            status: RoundStatus::Pending,
            bidding_start: 1000,
            bidding_end: 2000,
            created_at: 500,
            ..Default::default()
        };

        assert_eq!(
            round.assert_can_contribute(500),
            err!(RoundError::BiddingNotStarted)
        );

        assert_eq!(round.assert_can_contribute(1500), Ok(()));

        assert_eq!(
            round.assert_can_contribute(2500),
            err!(RoundError::BiddingEnded)
        );
    }

    #[test]
    fn test_can_accept_reject() {
        let mut round = Round {
            status: RoundStatus::Pending,
            bidding_start: 1000,
            bidding_end: 2000,
            ..Default::default()
        };

        assert_eq!(
            round.assert_can_accept_or_reject(500),
            err!(RoundError::BiddingStillGoing)
        );
        assert_eq!(
            round.assert_can_accept_or_reject(1500),
            err!(RoundError::BiddingStillGoing)
        );

        assert_eq!(
            round.assert_can_accept_or_reject(2001 + WITHDRAWAL_ENABLED_AFTER_INACTIVITY_TIMEOUT),
            err!(RoundError::OfferTimedOut)
        );

        round.status = RoundStatus::Accepted;
        assert_eq!(
            round.assert_can_accept_or_reject(1500),
            err!(RoundError::OfferIsNotPending),
        );

        round.status = RoundStatus::Rejected;
        assert_eq!(
            round.assert_can_accept_or_reject(1500),
            err!(RoundError::OfferIsNotPending),
        );
    }

    #[test]
    fn test_can_withdraw_pending() {
        let round = Round {
            status: RoundStatus::Pending,
            bidding_start: 1000,
            bidding_end: 2000,
            created_at: 500,
            ..Default::default()
        };

        assert_eq!(
            round.assert_can_withdraw(700, true),
            err!(RoundError::BiddingNotStarted)
        );

        assert_eq!(round.assert_can_withdraw(1500, true), Ok(()));
        assert_eq!(
            round.assert_can_withdraw(1500, false),
            err!(RoundError::CantWithdrawWithoutUserSignature)
        );

        assert_eq!(
            round.assert_can_withdraw(2100, true),
            err!(RoundError::InactivityTimeoutHasNotPassed)
        );

        assert_eq!(
            round.assert_can_withdraw(2100 + WITHDRAWAL_ENABLED_AFTER_INACTIVITY_TIMEOUT, false),
            Ok(())
        );
    }

    #[test]
    fn test_can_reject_user_bid() {
        let mut round = Round {
            status: RoundStatus::Pending,
            bidding_start: 1000,
            bidding_end: 2000,
            created_at: 500,
            ..Default::default()
        };

        assert_eq!(
            round.assert_can_reject_bid(700),
            err!(RoundError::BiddingStillGoing)
        );

        assert_eq!(
            round.assert_can_reject_bid(1500),
            err!(RoundError::BiddingStillGoing)
        );

        assert_eq!(round.assert_can_reject_bid(2001), Ok(()));

        round.status = RoundStatus::Accepted;
        assert_eq!(
            round.assert_can_reject_bid(2001),
            err!(RoundError::OfferIsNotPending)
        );
    }

    proptest! {
        #[test]
        fn proptest_withdraw_accepted(now in 2001i64..10000000000000, sig: bool) {
             let round = Round {
                    status: RoundStatus::Accepted,
                    bidding_start: 1000,
                    bidding_end: 2000,
                    created_at: 500,
                    ..Default::default()
                };

            assert_eq!(
                round.assert_can_withdraw(now, sig),
                err!(RoundError::CantWithdrawAcceptedOffer)
            );
        }

        #[test]
        fn proptest_can_withdraw_rejected(now in 2001i64..10000000000000, sig: bool) {
            let round = Round {
                status: RoundStatus::Rejected,
                bidding_start: 1000,
                bidding_end: 2000,
                created_at: 500,
                ..Default::default()
            };

            assert_eq!(round.assert_can_withdraw(now, sig), Ok(()));
        }
    }
}
