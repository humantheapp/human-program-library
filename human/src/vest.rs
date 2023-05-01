use crate::error::Error;
use crate::state::ContractState;
use crate::{authority, consts::*, contract_vault};

use borsh::{BorshDeserialize, BorshSerialize};

use human_common::entity::next_entity;
use solana_program::clock::{Clock, UnixTimestamp};
use solana_program::msg;
use solana_program::program_pack::Pack;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::Sysvar,
};
use spl_associated_token_account::get_associated_token_address;
use spl_math::precise_number::PreciseNumber;

#[derive(Debug, BorshDeserialize, BorshSerialize)]
pub struct VestState {
    /// created at
    pub deployed_at: UnixTimestamp,
    /// periods already vested. Max periods is hardcoded
    pub vested_periods: u8,
}

#[derive(Debug, PartialEq, Eq)]
pub enum VestingResult {
    // User part in this period
    Vest(u64),
    /// Already vested in this period
    PeriodVested,
    /// Vesting completed (vested_periods == VESTING_PARTS)
    Completed,
}

impl VestState {
    fn next_period(
        &mut self,
        vault_supply: u64,
        now: UnixTimestamp,
    ) -> Result<VestingResult, ProgramError> {
        if self.vested_periods == VESTING_TOTAL_PARTS {
            return Ok(VestingResult::Completed);
        }

        let elapsed = now.checked_sub(self.deployed_at).ok_or(Error::Overflow)?;

        let elapsed = PreciseNumber::new(elapsed as u128).unwrap();
        let part_len = PreciseNumber::new(VESTING_PART_LENGTH as u128).unwrap();

        let elapsed_parts = elapsed
            .checked_div(&part_len)
            .ok_or(Error::Overflow)?
            .floor()
            .ok_or(Error::Overflow)?;

        let elapsed_parts = elapsed_parts.to_imprecise().ok_or(Error::Overflow)?;
        let elapsed_parts = elapsed_parts.min(VESTING_TOTAL_PARTS as u128) as u8;

        let missed_parts = elapsed_parts.saturating_sub(self.vested_periods);

        if missed_parts == 0 {
            return Ok(VestingResult::PeriodVested);
        }

        let remaining_periods = VESTING_TOTAL_PARTS
            .checked_sub(self.vested_periods)
            .unwrap();

        // equal parts
        let part = vault_supply
            .checked_div(remaining_periods as u64)
            .ok_or(Error::Overflow)?;

        let amount = part
            .checked_mul(missed_parts as u64)
            .ok_or(Error::Overflow)?;

        self.vested_periods = self
            .vested_periods
            .checked_add(missed_parts)
            .ok_or(Error::Overflow)?;

        Ok(VestingResult::Vest(amount))
    }
}

// [write] state account
// [write] vault account
// [write] owner token wallet
// [] transfer authority
// [] token prog
pub fn process_vest(program_id: &Pubkey, accounts: &[AccountInfo]) -> Result<(), ProgramError> {
    let account_info_iter = &mut accounts.iter();

    let (mut state, _) = next_entity::<_, ContractState>(account_info_iter, program_id)?;

    let (vault_addr, _) = contract_vault!(program_id, state.token);
    let (_, vault_acc) = next_expected_token_wallet(account_info_iter, &vault_addr)?;

    let owner_token_wallet = next_account_info(account_info_iter)?;
    let transfer_authority_info = next_account_info(account_info_iter)?;

    let token_program = next_account_info(account_info_iter)?;

    let derived_owner_wallet = get_associated_token_address(&state.owner, &state.token);
    if *owner_token_wallet.key != derived_owner_wallet {
        return Err(ProgramError::InvalidArgument);
    }

    if !spl_token::check_id(token_program.key) {
        return Err(ProgramError::InvalidArgument);
    }

    let (transfer_authority, transfer_seed) = authority!(program_id);

    if *transfer_authority_info.key != transfer_authority {
        return Err(ProgramError::InvalidSeeds);
    }

    let now = Clock::get()?.unix_timestamp;

    let amount_to_transfer = match state.vest.next_period(vault_acc.amount, now)? {
        VestingResult::Vest(amount) => amount,
        VestingResult::PeriodVested => {
            msg!("vesting already occured for the period");
            return Ok(());
        }
        VestingResult::Completed => {
            msg!("vesting completed");
            return Ok(());
        }
    };

    let inst = spl_token::instruction::transfer(
        &spl_token::ID,
        &vault_addr,
        owner_token_wallet.key,
        transfer_authority_info.key,
        &[],
        amount_to_transfer,
    )?;

    invoke_signed(&inst, accounts, &[transfer_seed])?;

    Ok(())
}

pub fn next_expected_token_wallet<'a, 'b: 'a, I>(
    i: &mut I,
    wallet_addr: &Pubkey,
) -> Result<(Pubkey, spl_token::state::Account), ProgramError>
where
    I: Iterator<Item = &'a AccountInfo<'b>>,
{
    let wallet = next_account_info(i)?;

    if wallet.key != wallet_addr {
        msg!("invalid expected wallet");
        return Err(ProgramError::InvalidArgument);
    }

    let account = spl_token::state::Account::unpack(&wallet.data.borrow())?;

    Ok((*wallet.key, account))
}

#[cfg(test)]
mod tests {
    use rand::Rng;

    use super::*;
    use crate::consts::VESTING_PART_LENGTH;
    use proptest::prelude::*;

    fn test_vesting_fuzz(supply: u64) {
        let mut vault_balance = supply;
        let mut now = 0;

        let mut sent = Vec::new();
        let mut i = 0;

        let mut vest = VestState {
            deployed_at: 0,
            vested_periods: 0,
        };

        loop {
            i += 1;
            let min = VESTING_PART_LENGTH / 2;
            let max = VESTING_PART_LENGTH * 2 + 1000;
            now += rand::thread_rng().gen_range(min..max);

            match vest.next_period(vault_balance, now).unwrap() {
                VestingResult::Vest(amount) => {
                    println!("{i}: sent {amount}");
                    sent.push(amount);

                    vault_balance -= amount;
                }
                VestingResult::PeriodVested => continue,
                VestingResult::Completed => break,
            }
        }

        println!("sent ({}) = {:#?}", sent.len(), sent);

        let seconds_in_year = 60 * 60 * 24 * 365;
        dbg!(
            now / seconds_in_year,
            (VESTING_PART_LENGTH * VESTING_TOTAL_PARTS as i64) / seconds_in_year
        );
        dbg!(now, VESTING_PART_LENGTH * VESTING_TOTAL_PARTS as i64);

        // Completion should have occured somewhere between sum of all periods to allow for some errors
        assert!(now > VESTING_PART_LENGTH * VESTING_TOTAL_PARTS as i64);

        let deadline = VESTING_PART_LENGTH * (VESTING_TOTAL_PARTS as i64 + 4);
        assert!(
            now < deadline,
            "difference of {} days",
            (deadline - now).abs() as f64 / 60.0 / 60.0 / 24.0
        );

        assert!(sent.len() as u8 <= VESTING_TOTAL_PARTS);

        const EPSILON: u64 = 1; // allow for one chatlan of accuracy
        assert!(supply - sent.iter().sum::<u64>() <= EPSILON, "total sent");
        assert!(vault_balance <= EPSILON);
    }

    proptest! {
        #[test]
        fn proptest_vesting(supply: u64) {
            test_vesting_fuzz(supply);
        }
    }

    #[test]
    fn test_vesting_normal() {
        let mut vs = VestState {
            deployed_at: 0,
            vested_periods: 0,
        };

        let result = vs.next_period(36000, VESTING_PART_LENGTH + 1).unwrap();

        assert_eq!(result, VestingResult::Vest(1000)); // 36000 / 36 (vesting parts)
        assert_eq!(vs.vested_periods, 1);

        // transition to complete
        let mut vs = VestState {
            deployed_at: 0,
            vested_periods: VESTING_TOTAL_PARTS - 1,
        };

        let result = vs
            .next_period(1337, VESTING_PART_LENGTH * VESTING_TOTAL_PARTS as i64 + 1)
            .unwrap();

        assert_eq!(result, VestingResult::Vest(1337));
        assert_eq!(vs.vested_periods, VESTING_TOTAL_PARTS);

        // multiple periods elapsed
        assert_eq!(
            VestState {
                deployed_at: 0,
                vested_periods: 0,
            }
            .next_period(36000, VESTING_PART_LENGTH * 2)
            .unwrap(),
            VestingResult::Vest(2000)
        );
    }

    #[test]
    fn test_vesting_already_vested() {
        let mut vs = VestState {
            deployed_at: 0,
            vested_periods: 1,
        };

        let result = vs.next_period(9999, VESTING_PART_LENGTH + 1000).unwrap();

        assert_eq!(result, VestingResult::PeriodVested);

        assert_eq!(
            VestState {
                deployed_at: 0,
                vested_periods: 1,
            }
            .next_period(9999, 1)
            .unwrap(),
            VestingResult::PeriodVested
        );
    }

    #[test]
    fn test_vesting_completed() {
        let mut vs = VestState {
            deployed_at: 0,
            vested_periods: VESTING_TOTAL_PARTS,
        };

        let result = vs.next_period(9999, 9999).unwrap();
        assert_eq!(result, VestingResult::Completed);
    }

    #[test]
    fn test_vesting_weird() {
        // no vesting occured for a lifetime of a contract
        let mut vs = VestState {
            deployed_at: 0,
            vested_periods: 0,
        };

        let result = vs.next_period(360000, i64::MAX).unwrap();
        assert_eq!(vs.vested_periods, VESTING_TOTAL_PARTS);
        assert_eq!(result, VestingResult::Vest(360000));
    }
}
