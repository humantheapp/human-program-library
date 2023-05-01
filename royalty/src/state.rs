use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{program_error::ProgramError, pubkey::Pubkey};
use spl_math::precise_number::PreciseNumber;

use crate::error::Error;
use human_common::entity::Entity;

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[must_use]
pub struct State {
    // token wallet associated with this state
    pub wallet: Pubkey,
    pub owner: Pubkey,
    pub host: Pubkey,

    /// total amount distributed on the contract
    pub total_distributed: u64,
    /// total amount distributed minus host and owner split
    pub total_user_distributed: u64,

    pub settings: Settings,

    /// drop_idx is monotonically growing counter to track redeemed voucher
    /// voucher could only redeemed if it's drop_idx matches state's one
    /// on redeem it's drop_idx is incremented, so it can only be redeemed in next drop.
    /// Vouchers are created with current drop_idx, but since they can't be
    /// issued in the middle of distribution, we are safe
    pub drop_idx: u32,

    /// total vouchers in circulation
    pub vouchers_count: u32,

    /// total token balance in stake.
    /// counted separately to prevent someone from sending tokens directly
    /// to wallet and breaking math in the middle of distribution
    /// these tokens will also reduce everyone's share
    pub tokens_held: u64,

    /// current distribution
    pub distribution: Option<Distribution>,

    /// Staking token mint
    pub token_mint: Pubkey,

    /// While forming voting record, use this field
    pub realm_addr: Pubkey,

    /// Vault addr to add weight to creator vote
    pub vault_addr: Pubkey,
}

impl Entity for State {
    // Hardcoded size to allow for future migrations
    const SIZE: usize = 512;
    const MAGIC: u8 = 0x77;
}

#[derive(Debug, PartialEq, Eq)]
pub struct Split {
    pub distribute_amount: u64,
    pub owner_comission: u64,
    pub host_comission: u64,
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
pub struct Settings {
    /// minimum amount of tokens for user
    pub min_token_to_enroll: u64,
    /// owner commission % in basis points
    pub owner_fee: u16,
    /// host (us) commission % in basis points
    /// https://www.investopedia.com/terms/b/basispoint.asp
    pub host_fee: u16,
    /// host per user flat comission in lamports
    pub host_flat_fee: u32,
}

impl Settings {
    pub fn valid(&self) -> bool {
        if self.owner_fee.saturating_add(self.host_fee) >= 10000 {
            return false;
        }

        true
    }

    pub fn calculate_split(&self, balance: u64, num_vouchers: u32) -> Option<Split> {
        let bsp: PreciseNumber = PreciseNumber::new(10_000).unwrap();

        let balance_precise = PreciseNumber::new(balance as u128)?;
        let owner_fee = PreciseNumber::new(self.owner_fee as u128)?;
        let host_fee = PreciseNumber::new(self.host_fee as u128)?;

        // balance * 1500/10000 if comission is 15%
        let owner_split: u64 = balance_precise
            .checked_mul(&owner_fee)?
            .checked_div(&bsp)?
            .to_imprecise()?
            .try_into()
            .ok()?;

        dbg!(owner_split);

        let host_split: u64 = balance_precise
            .checked_mul(&host_fee)?
            .checked_div(&bsp)?
            .to_imprecise()?
            .try_into()
            .ok()?;

        let host_flat_fee = self.host_flat_fee.checked_mul(num_vouchers)?;

        let host_total_comission = host_split.checked_add(host_flat_fee as u64)?;

        let remaining_balance = balance
            .checked_sub(owner_split)?
            .checked_sub(host_total_comission)?;

        Some(Split {
            distribute_amount: remaining_balance,
            owner_comission: owner_split,
            host_comission: host_total_comission,
        })
    }
}

#[derive(Debug, BorshDeserialize, BorshSerialize, Clone)]
pub struct Distribution {
    pub distribute_amount: u64,
    pub seen_vouchers: u32,
}

impl Distribution {
    /// calculate distribution for this address. returns amount of lamports to transfer to user
    pub fn distribute_to(
        &mut self,
        voucher: &mut Voucher,
        drop_idx: u32,
        total_tokens: u64,
    ) -> Result<u64, ProgramError> {
        if voucher.drop_idx != drop_idx {
            return Error::VoucherNotEligible.into();
        }

        // two cases where vouches would not be valid for current drop:
        // 1. Voucher was created during distribution phase, so it has not been enumerated
        // 2. Voucher was already redeeemed (distributed to)
        let balance = PreciseNumber::new(voucher.balance as u128).unwrap();
        let total_tokens = PreciseNumber::new(total_tokens as u128).unwrap();
        let distribute_amount = PreciseNumber::new(self.distribute_amount as u128).unwrap();

        // TODO calculation precision
        // e.g. 0.25 (25%) of total tokens
        let owned_percentage = balance.checked_div(&total_tokens).ok_or(Error::Overflow)?;
        assert!(owned_percentage.less_than_or_equal(&PreciseNumber::new(1).unwrap()));

        // part of total tokens
        let part = distribute_amount
            .checked_mul(&owned_percentage)
            .ok_or(Error::Overflow)?;

        self.seen_vouchers = self.seen_vouchers.checked_add(1).ok_or(Error::Overflow)?;

        let lamports = part
            .to_imprecise()
            .ok_or(Error::Overflow)?
            .try_into()
            .map_err(|_| Error::Overflow)?;

        voucher.drop_idx = voucher.drop_idx.checked_add(1).ok_or(Error::Overflow)?;

        Ok(lamports)
    }
}

pub type Voucher = VoucherV2;

#[derive(Debug, BorshDeserialize, BorshSerialize)]
pub struct VoucherV2 {
    pub user: Pubkey,
    pub state: Pubkey,
    pub drop_idx: u32,
    pub balance: u64,
}

impl Entity for VoucherV2 {
    const SIZE: usize = 128;
    const MAGIC: u8 = 0x55;
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn test_split() {
        let s = Settings {
            min_token_to_enroll: 1000,
            owner_fee: 1000,
            host_fee: 500,
            host_flat_fee: 5000,
        };

        assert_eq!(
            s.calculate_split(1_000_000, 5).unwrap(),
            Split {
                owner_comission: 100_000,
                host_comission: 50000 + 25000,
                distribute_amount: 825_000,
            }
        );

        // make sure weird values are calculated in a sane way
        assert_eq!(
            s.calculate_split(3_333_333, 5).unwrap(),
            Split {
                owner_comission: 333333,
                host_comission: 166667 + 25000,
                distribute_amount: 2808333,
            }
        );
    }

    #[test]
    fn test_distribute() {
        let mut ds = Distribution {
            distribute_amount: 100_000,
            seen_vouchers: 0,
        };

        let total_tokens = 1000;
        let drop_idx = 123;

        let mut v1 = Voucher {
            user: Pubkey::new_unique(),
            state: Pubkey::new_unique(),
            balance: 500,
            drop_idx,
        };

        assert_eq!(
            ds.distribute_to(&mut v1, drop_idx, total_tokens).unwrap(),
            50_000
        );
        assert_eq!(v1.drop_idx, drop_idx + 1);
        assert_eq!(ds.seen_vouchers, 1);

        let mut v2 = Voucher {
            user: Pubkey::new_unique(),
            state: Pubkey::new_unique(),
            balance: 250,
            drop_idx,
        };

        assert_eq!(
            ds.distribute_to(&mut v2, drop_idx, total_tokens).unwrap(),
            25_000
        );

        // can't redeem same voucher twice
        ds.distribute_to(&mut v2, drop_idx, total_tokens)
            .unwrap_err();
        assert_eq!(v2.drop_idx, drop_idx + 1);
        assert_eq!(ds.seen_vouchers, 2);

        // test invalid voucher
        let mut prev_idx_voucher = Voucher {
            user: Pubkey::new_unique(),
            state: Pubkey::new_unique(),
            balance: 250,
            drop_idx: drop_idx - 1,
        };
        ds.distribute_to(&mut prev_idx_voucher, drop_idx, total_tokens)
            .unwrap_err();

        let mut v3 = Voucher {
            user: Pubkey::new_unique(),
            state: Pubkey::new_unique(),
            balance: 250,
            drop_idx,
        };
        assert_eq!(
            ds.distribute_to(&mut v3, drop_idx, total_tokens).unwrap(),
            25_000
        );
        assert_eq!(v3.drop_idx, drop_idx + 1);
        assert_eq!(ds.seen_vouchers, 3);
    }

    proptest! {
        #[test]
        fn proptest_calculate_split(
            of in 0u16..8000,
            hf in 0u16..1000,
            hff in 0u32..100000,
            lamports: u64,
        ) {
            let settings = Settings {
                min_token_to_enroll: 1_0000,
                owner_fee: of,
                host_fee: hf,
                host_flat_fee: hff,
            };

            let split = settings.calculate_split(lamports, 10).unwrap();

            assert_eq!(split.distribute_amount + split.host_comission + split.owner_comission, lamports)
        }

        #[test]
        fn proptest_distribute(
            distribution_amount: u64, (total_tokens, mut vouchers) in vouchers(),
        ) {
            proptest_distribute_inner(distribution_amount, total_tokens, &mut vouchers);
        }
    }

    prop_compose! {
        fn vouchers()(vec in prop::collection::vec(0u64..100000, 1..100)) -> (u64, Vec<Voucher>) {
            let vouchers = vec.iter().map(|amount| Voucher { user: Pubkey::new_unique(), state: Pubkey::new_unique(), drop_idx: 1, balance: *amount }).collect();
            (vec.iter().sum(), vouchers)
        }
    }

    fn proptest_distribute_inner(
        total_amount: u64,
        total_tokens: u64,
        vouchers: &mut Vec<Voucher>,
    ) {
        let mut x = Distribution {
            distribute_amount: total_amount,
            seen_vouchers: 0,
        };

        let l = vouchers.len();
        let mut amounts = Vec::new();
        for v in vouchers {
            amounts.push(x.distribute_to(v, 1, total_tokens).unwrap());
        }

        let sum = amounts.iter().copied().sum::<u64>() as i64;
        let diff = sum.checked_sub(total_amount as i64).unwrap().abs();
        println!("{} {} {}", sum, total_amount, diff);
        assert!(diff as f64 <= total_amount as f64 * 0.0001);
        assert_eq!(x.seen_vouchers as usize, l);
    }
}
