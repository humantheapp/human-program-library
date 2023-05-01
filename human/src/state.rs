use crate::error::Error;
use crate::vest::*;
use crate::{consts::*, InitInstruction};
use borsh::{BorshDeserialize, BorshSerialize};
use human_common::entity::Entity;
use solana_program::program_memory::sol_memcpy;
use solana_program::{clock::UnixTimestamp, msg, program_error::ProgramError, pubkey::Pubkey};
use spl_math::precise_number::PreciseNumber;

/// Since accounts can't be resized this is constant size
/// to allow some headroom for future migrations
pub const STATE_ACC_SIZE: usize = 512 + 1;

pub type ContractState = ContractStateV4;

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(C)]
pub struct ContractStateV4 {
    /// Associated token mint
    pub token: Pubkey,
    /// Alice
    pub owner: Pubkey,
    /// required for priveleged operations
    pub admin: Pubkey,
    /// comission WSOL account
    pub commission_addr: Pubkey,
    /// owner treasury
    pub treasury_addr: Pubkey,
    /// swap state
    pub swap_state: Pubkey,
    /// tokens sold
    pub sold: u64,
    /// vesting state
    pub vest: VestState,
    /// in progress drop. use helper methods to access this field
    pub drop: Option<DropV2>,
    /// round in progress, used for vesting 10% of tokens
    pub current_round: Option<Pubkey>,
    /// completed rounds count
    pub completed_rounds_count: u64,
}

impl Entity for ContractStateV4 {
    const SIZE: usize = STATE_ACC_SIZE;
    const MAGIC: u8 = 0x45;
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(C)]
pub struct DropV2 {
    pub id: u64,
    /// price per chatlan in lamports
    pub price: u64,
    pub amount: u64,
    pub created_at: UnixTimestamp,
    pub start_date: UnixTimestamp,
    pub end_date: UnixTimestamp,
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(C)]
pub struct Split {
    /// price per chatlan in lamports
    pub percent: u16,
    pub split_addr: Pubkey,
}

pub struct BuySplit {
    pub owner_split: u64,
    pub commission: u64,
    pub treasury_split: u64,

    pub token_commission: u64,
}

impl ContractState {
    pub fn calculate_buy_split(
        &self,
        now: UnixTimestamp,
        token_amount: u64,
        expected_price: u64,
    ) -> Result<BuySplit, ProgramError> {
        let price_per_chatlan = self.get_ongoing_drop_price(now)?;

        if price_per_chatlan > expected_price {
            return Error::ExpectedPriceMismatch.into();
        }

        let split =
            Self::calculate_split(price_per_chatlan, token_amount).ok_or(Error::Overflow)?;

        Ok(split)
    }

    fn calculate_split(price_per_chatlan: u64, token_amount: u64) -> Option<BuySplit> {
        let lamports = price_per_chatlan.checked_mul(token_amount)?;

        let tokens_precise = PreciseNumber::new(token_amount as u128)?;
        let commission_percent = PreciseNumber::new(BUY_COMMISSION as u128)?;

        let bsp = PreciseNumber::new(10_000)?;
        let token_commission: u64 = tokens_precise
            .checked_mul(&commission_percent)?
            .checked_div(&bsp)?
            .to_imprecise()?
            .try_into()
            .ok()?;

        let mut split = Self::calculate_split_by_lamports(lamports)?;

        split.token_commission = token_commission;

        Some(split)
    }

    pub fn calculate_split_by_lamports(lamports: u64) -> Option<BuySplit> {
        let lamports_precise = PreciseNumber::new(lamports as u128)?;

        let bsp = PreciseNumber::new(10_000)?;
        let commission_percent = PreciseNumber::new(BUY_COMMISSION as u128)?;

        let additional_split_percent = PreciseNumber::new(TREASURY_COMMISSION as u128)?;

        let commission: u64 = lamports_precise
            .checked_mul(&commission_percent)?
            .checked_div(&bsp)?
            .to_imprecise()?
            .try_into()
            .ok()?;

        let treasury_split: u64 = lamports_precise
            .checked_mul(&additional_split_percent)?
            .checked_div(&bsp)?
            .to_imprecise()?
            .try_into()
            .ok()?;

        let owner_split = lamports
            .checked_sub(commission)?
            .checked_sub(treasury_split)?;

        Some(BuySplit {
            commission,
            owner_split,
            treasury_split,
            token_commission: 0,
        })
    }

    /// try to get current drop price and validate if time window matches current time
    fn get_ongoing_drop_price(&self, now: UnixTimestamp) -> Result<u64, ProgramError> {
        let drop = self.drop.as_ref().ok_or(Error::NoDrop)?;

        if now < drop.start_date || now > drop.end_date {
            msg!(
                "missed drop time frame: start = {}, now = {}, end = {}",
                drop.start_date,
                now,
                drop.end_date
            );
            return Error::DropTimeframeExpired.into();
        }

        Ok(drop.price)
    }

    pub fn create_drop(
        &mut self,
        price: u64,
        id: u64,
        amount: u64,
        now: UnixTimestamp,
        start_date: UnixTimestamp,
        end_date: UnixTimestamp,
    ) -> Result<(), ProgramError> {
        if self.drop.is_some() {
            // drop already in progress
            return Error::DropInvalidDate.into();
        }

        if price == 0 {
            return Error::DropPriceZero.into();
        }

        if end_date <= start_date {
            return Error::DropInvalidDate.into();
        }

        self.drop = Some(DropV2 {
            id,
            amount,
            price,
            start_date,
            end_date,
            created_at: now,
        });

        Ok(())
    }

    pub fn clear_drop(&mut self) {
        self.drop = None;
    }
}

pub fn try_migrate_state(
    data: &mut [u8],
    swap_state: Pubkey,
    new_commission: Pubkey,
    treasury: Pubkey,
    now: UnixTimestamp,
) -> Result<(), ProgramError> {
    if data.is_empty() {
        return Err(ProgramError::InvalidAccountData);
    }

    let (magic, _payload) = (&data[0], &data[1..]);

    match *magic {
        ContractStateV4::MAGIC => {
            msg!("already up to date on v4");
            Ok(())
        }
        ContractStateV3::MAGIC => {
            msg!("migrating from v3 to v4");
            let v3 = ContractStateV3::deserialize_from(data)?;
            let v4 = v3
                .migrate(swap_state, new_commission, treasury, now)
                .try_to_vec()?;

            data[0] = ContractStateV4::MAGIC;
            copy_slice(&mut data[1..], &v4);

            Ok(())
        }
        _ => {
            msg!("invalid state");
            Err(ProgramError::InvalidAccountData)
        }
    }
}

pub fn init_state(
    data: &mut [u8],
    token: Pubkey,
    args: InitInstruction,
    now: UnixTimestamp,
) -> Result<(), ProgramError> {
    if data.is_empty() {
        return Err(ProgramError::AccountDataTooSmall);
    }

    if data[0] != 0 {
        return Err(ProgramError::AccountAlreadyInitialized);
    }

    let state = ContractState {
        token,
        owner: args.owner,
        admin: args.admin,
        commission_addr: args.commission,
        vest: VestState {
            deployed_at: now,
            vested_periods: 0,
        },
        drop: None,
        sold: 0,
        swap_state: args.swap_state,
        treasury_addr: args.treasury,
        current_round: None,
        completed_rounds_count: 0,
    };

    if ContractState::is_initialized(data) {
        return Err(ProgramError::AccountAlreadyInitialized);
    }

    state.serialize_to(data)?;

    Ok(())
}

#[inline]
fn copy_slice(dst: &mut [u8], src: &[u8]) {
    sol_memcpy(dst, src, src.len())
}

// old
#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(C)]
pub struct ContractStateV3 {
    /// Associated token mint
    pub token: Pubkey,
    /// Alice
    pub owner: Pubkey,
    /// required for priveleged operations
    pub admin: Pubkey,
    /// comission account, could be changed by the admin
    pub commission_addr: Pubkey,
    /// tokens sold
    pub sold: u64,
    /// vesting state
    pub vest: VestState,
    /// in progress drop. use helper methods to access this field
    pub drop: Option<DropV1>,
    /// additional split
    pub additional_split: Option<Split>,
}

impl Entity for ContractStateV3 {
    const SIZE: usize = STATE_ACC_SIZE;
    const MAGIC: u8 = 0x44;
}

impl ContractStateV3 {
    fn migrate(
        self,
        swap_state: Pubkey,
        new_commission: Pubkey,
        treasury: Pubkey,
        now: UnixTimestamp,
    ) -> ContractStateV4 {
        ContractStateV4 {
            token: self.token,
            owner: self.owner,
            admin: self.admin,
            commission_addr: new_commission,
            sold: self.sold,
            vest: self.vest,
            drop: self.drop.map(|d: DropV1| DropV2 {
                price: d.price,
                start_date: d.start_date,
                end_date: d.end_date,
                id: 0,
                amount: 1000_0000,
                created_at: now,
            }),
            swap_state,
            treasury_addr: self
                .additional_split
                .map(|s: Split| s.split_addr)
                .unwrap_or(treasury),
            current_round: None,
            completed_rounds_count: 0,
        }
    }
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(C)]
pub struct DropV1 {
    /// price per chatlan in lamports
    pub price: u64,
    pub start_date: UnixTimestamp,
    pub end_date: UnixTimestamp,
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(C)]
pub struct PostInfo {
    pub state: Pubkey, // state related to this
    pub post_id: [u8; 32],
    pub created_at: UnixTimestamp,
    pub repost_price: Option<u64>,
}

impl PostInfo {
    pub fn can_repost(&self, now: UnixTimestamp) -> bool {
        self.created_at
            .checked_sub(now)
            .map(|elapsed| elapsed < MAX_REPOST_TIME)
            .unwrap_or(false)
    }
}

impl Entity for PostInfo {
    const SIZE: usize = 128;
    const MAGIC: u8 = 0x44;
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(C)]
pub struct RepostRecord {
    pub state: Pubkey, // state related to this
    pub token: Pubkey, // token to receive
    pub user: Pubkey,  // user that made the repost
    pub post_id: [u8; 32],
    pub reposted_at: UnixTimestamp,
    pub receive_amount: u64,
}

impl RepostRecord {
    pub fn can_redeem(&self, now: UnixTimestamp) -> bool {
        let cooldown_elapsed = self
            .reposted_at
            .checked_sub(now)
            .map(|elapsed| elapsed < REPOST_REDEEM_COOLDOWN)
            .unwrap_or(false);

        // thanks https://stackoverflow.com/questions/37847020/unix-time-stamp-day-of-the-week-pattern#38000383
        let week_progress = ((now - 345600) % 604800) as f64 / 86400.0;

        let is_thursday = week_progress > 3.0 && week_progress < 4.0;

        cooldown_elapsed && is_thursday
    }
}

impl Entity for RepostRecord {
    const SIZE: usize = 145;
    const MAGIC: u8 = 0x40;
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn proptest_vesting(price in 1u64..100000000000, token_amount in 0u64..10000_0000) {
            ContractState::calculate_split(price, token_amount).unwrap();
        }
    }
}
