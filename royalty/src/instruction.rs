use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
    sysvar::{self, rent},
};
use spl_associated_token_account::get_associated_token_address;

use crate::{max_weight_record, state::Settings, voucher, wallet, weight_record};

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(u8)]
#[non_exhaustive]
pub enum RoyaltyInstruction {
    /// Initialize royalty state
    Initialize(InitializeArgs),

    /// Start distribution
    Distribute,

    /// Claim distribution rewards
    Claim,

    /// Deposit tokens
    DepositTokens(u64),

    /// Withdraw
    WithdrawTokens(u64),

    /// Sync voting record
    SyncWeightRecord,

    Migrate(MigrateArgs),
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
pub struct MigrateArgs {
    pub realm_addr: Pubkey,
    pub vault_addr: Pubkey,
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
pub struct InitializeArgs {
    pub owner: Pubkey,
    pub host: Pubkey,
    pub settings: Settings,
    pub realm_addr: Pubkey,
    pub vault_addr: Pubkey,
}

pub fn initialize(
    program_id: &Pubkey,
    state: &Pubkey,
    mint: &Pubkey,
    fee_payer: &Pubkey,
    args: InitializeArgs,
) -> Instruction {
    let (wallet_addr, _) = wallet!(program_id, state);

    let accounts = vec![
        AccountMeta::new(*state, false),
        AccountMeta::new_readonly(*mint, false),
        AccountMeta::new(wallet_addr, false),
        AccountMeta::new(*fee_payer, true),
        AccountMeta::new_readonly(system_program::ID, false),
        AccountMeta::new_readonly(spl_token::ID, false),
        AccountMeta::new_readonly(rent::ID, false),
    ];

    Instruction {
        program_id: *program_id,
        accounts,
        data: RoyaltyInstruction::Initialize(args).try_to_vec().unwrap(),
    }
}

pub fn deposit(
    program_id: &Pubkey,
    state: &Pubkey,
    vault: &Pubkey,
    mint: &Pubkey,
    user: &Pubkey,
    owner: &Pubkey,
    fee_payer: &Pubkey,
    amount: u64,
) -> Instruction {
    let (wallet_addr, _) = wallet!(program_id, state);
    let (voucher_addr, _) = voucher!(program_id, state, user);
    let (weight_record, _) = max_weight_record!(program_id, state);
    let atoken = get_associated_token_address(user, mint);

    let owner_atoken = get_associated_token_address(owner, mint);

    let accounts = vec![
        AccountMeta::new(*state, false),
        AccountMeta::new(wallet_addr, false),
        AccountMeta::new(voucher_addr, false),
        AccountMeta::new_readonly(*user, true),
        AccountMeta::new(atoken, false),
        AccountMeta::new_readonly(*vault, false),
        AccountMeta::new_readonly(owner_atoken, false),
        AccountMeta::new(weight_record, false),
        AccountMeta::new(*fee_payer, true),
        AccountMeta::new_readonly(system_program::ID, false),
        AccountMeta::new_readonly(spl_token::ID, false),
        AccountMeta::new_readonly(sysvar::instructions::ID, false),
    ];

    Instruction {
        program_id: *program_id,
        accounts,
        data: RoyaltyInstruction::DepositTokens(amount)
            .try_to_vec()
            .unwrap(),
    }
}

pub fn withdraw(
    program_id: &Pubkey,
    state: &Pubkey,
    vault: &Pubkey,
    mint: &Pubkey,
    user: &Pubkey,
    owner: &Pubkey,
    token_owner_record: &Pubkey,
    fee_payer: &Pubkey,
    amount: u64,
) -> Instruction {
    let (wallet_addr, _) = wallet!(program_id, state);
    let (voucher_addr, _) = voucher!(program_id, state, user);
    let (weight_record, _) = weight_record!(program_id, state, user);
    let (max_weight_record, _) = max_weight_record!(program_id, state);
    let atoken = get_associated_token_address(user, mint);

    let owner_atoken = get_associated_token_address(owner, mint);

    let accounts = vec![
        AccountMeta::new(*state, false),
        AccountMeta::new(wallet_addr, false),
        AccountMeta::new(voucher_addr, false),
        AccountMeta::new_readonly(*user, true),
        AccountMeta::new(atoken, false),
        AccountMeta::new_readonly(*token_owner_record, false),
        AccountMeta::new(weight_record, false),
        AccountMeta::new_readonly(*vault, false),
        AccountMeta::new_readonly(owner_atoken, false),
        AccountMeta::new(max_weight_record, false),
        AccountMeta::new(*fee_payer, true),
        AccountMeta::new_readonly(spl_token::ID, false),
        AccountMeta::new_readonly(system_program::ID, false),
    ];

    Instruction {
        program_id: *program_id,
        accounts,
        data: RoyaltyInstruction::WithdrawTokens(amount)
            .try_to_vec()
            .unwrap(),
    }
}

pub fn sync_weight_record(
    program_id: &Pubkey,
    state: &Pubkey,
    vault: &Pubkey,
    owner: &Pubkey,
    token: &Pubkey,
    user: &Pubkey,
    funder: &Pubkey,
) -> Instruction {
    let (voucher_addr, _) = voucher!(program_id, state, user);
    let (weight_record, _) = weight_record!(program_id, state, user);
    let (max_weight_record, _) = max_weight_record!(program_id, state);

    let owner_atoken = get_associated_token_address(owner, token);

    let accounts = vec![
        AccountMeta::new(*state, false),
        AccountMeta::new_readonly(*user, false),
        AccountMeta::new(voucher_addr, false),
        AccountMeta::new(weight_record, false),
        AccountMeta::new_readonly(*vault, false),
        AccountMeta::new_readonly(owner_atoken, false),
        AccountMeta::new(max_weight_record, false),
        AccountMeta::new(*funder, true),
        AccountMeta::new_readonly(system_program::ID, false),
    ];

    Instruction {
        program_id: *program_id,
        accounts,
        data: RoyaltyInstruction::SyncWeightRecord.try_to_vec().unwrap(),
    }
}
