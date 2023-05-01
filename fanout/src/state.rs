use std::collections::HashMap;

use anchor_lang::prelude::*;
use spl_math::precise_number::PreciseNumber;

#[account]
#[derive(Default)]
pub struct Fanout {
    /// some basic stats
    pub distributed: u64,
    pub members: Vec<Member>,
}

impl Fanout {
    pub fn calculate_split(&self, amount: u64) -> Option<HashMap<Pubkey, u64>> {
        let bsp: PreciseNumber = PreciseNumber::new(10_000).unwrap();

        let amount_precise = PreciseNumber::new(amount as u128)?;

        // each member gets (amount * share/10000)
        let mut split: HashMap<Pubkey, u64> = HashMap::new();

        for m in self.members.iter() {
            let share = PreciseNumber::new(m.share as u128)?;
            let share_amount = share.checked_mul(&amount_precise)?.checked_div(&bsp)?;

            split.insert(m.address, share_amount.to_imprecise()?.try_into().ok()?);
        }

        // fix differences due to rounding
        let total: u64 = split.values().sum();
        let diff = amount as i64 - total as i64;

        if diff != 0 {
            // some lucky member gets one less or one more lamport
            let v = split.get_mut(&self.members[0].address).unwrap();
            *v = (*v as i64).checked_add(diff)?.try_into().ok()?;
        }

        Some(split)
    }
}

#[derive(Default, Debug, Clone, AnchorSerialize, AnchorDeserialize, InitSpace, PartialEq, Eq)]
pub struct Member {
    pub address: Pubkey,
    pub share: u16,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    prop_compose! {
        fn arb_members()(len in 1usize..10) -> Vec<Member> {
            let mut shares = Vec::new();

            let target = 10000;
            let mut sum = 0;

            for i in 0..len {
                let x = (target - sum) / (len - i);
                sum += x;

                let m = Member {
                    address: Pubkey::new_unique(),
                    share: x as u16,
                };
                shares.push(m)
            }

            assert!(shares.iter().fold(0, |acc, m| acc + m.share as u64) == 10000);

            shares
        }
    }

    proptest! {
        #[test]
        fn proptest_split(amount in 10u64..u64::MAX, members in arb_members()) {
            let fanout = Fanout {
                members,
                ..Default::default()
            };
            let split = fanout.calculate_split(amount).unwrap();
            let total: u64 = split.values().sum();
            assert_eq!(total, amount);
        }
    }
}
