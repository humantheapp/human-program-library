#![forbid(unsafe_code)]
#![deny(clippy::all)]
#![allow(clippy::too_many_arguments)]
#![cfg(not(feature = "no-entrypoint"))]
pub mod consts;
pub mod error;
pub mod state;
pub mod vest;

use std::mem;

use crate::consts::*;
use crate::error::Error;
use crate::state::{
    init_state, try_migrate_state, ContractState, PostInfo, RepostRecord, STATE_ACC_SIZE,
};

use borsh::{BorshDeserialize, BorshSerialize};

use human_common::entity::{initialize_entity, next_entity, Entity};
use mpl_token_metadata::state::{CollectionDetails, Creator, TokenMetadataAccount};
use solana_program::clock::{Clock, UnixTimestamp};
use solana_program::program_memory::sol_memset;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint,
    entrypoint::ProgramResult,
    instruction::{self, AccountMeta},
    msg,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    program_option::COption,
    program_pack::Pack,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction, system_program,
    sysvar::{self, Sysvar},
};
use spl_associated_token_account::get_associated_token_address;
use spl_math::precise_number::PreciseNumber;
use spl_token::instruction::{close_account, initialize_account2};
use spl_token_swap::instruction as swap_instruction;

use human_common::utils::{
    next_atoken_wallet, next_expected_account, next_expected_token_wallet, next_signer_account,
};

use shank::ShankInstruction;

use anchor_lang::AccountDeserialize;
use anchor_lang::InstructionData;

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(C)]
pub struct InitInstruction {
    /// Alice
    pub owner: Pubkey,
    /// required for priveleged operations
    pub admin: Pubkey,
    /// comission account, could be changed by the admin
    pub commission: Pubkey,
    /// treasury comission
    pub treasury: Pubkey,
    /// swap to deposit liquidity to
    pub swap_state: Pubkey,
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(C)]
pub struct CreateDropInstruction {
    // some unique ID
    pub id: u64,
    // how much to sell
    pub amount: u64,
    // price per chatlan
    pub price: u64,

    pub start_date: UnixTimestamp,
    pub end_date: UnixTimestamp,
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(C)]
pub struct SetAdminInstruction {
    pub admin: Pubkey,
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(C)]
pub struct RegisterPostInstruction {
    pub royalty_addr: Pubkey,
    pub post_id: [u8; 32],
    pub created_at: UnixTimestamp,
    pub post_name: String,
    pub post_metadata_uri: String,
    pub collection_name: String,
    pub collection_metadata_uri: String,
    pub symbol: String,
    pub repost_price: u64,
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(C)]
pub struct RepostInstruction {}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(C)]
pub struct CreateRoundInstruction {
    bidding_start: UnixTimestamp,
    bidding_end: UnixTimestamp,
    offer_amount: u64,
    target_bid: u64,
}

#[derive(Debug, BorshDeserialize, BorshSerialize, ShankInstruction)]
#[repr(u8)]
#[non_exhaustive]
pub enum Instruction {
    CreateWallets,
    Init(InitInstruction),
    Deprecated1,
    Deprecated2,
    Deprecated3,
    Deprecated4,
    Vest,
    MigrateState,
    SetAdmin(SetAdminInstruction),
    DepositCommission,
    RegisterPost(RegisterPostInstruction),
    Repost(RepostInstruction),
    RedeemRepost,
    CreateRound(CreateRoundInstruction),
    ClaimRoundVesting,
}

entrypoint!(process_instruction);
pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let instruction = Instruction::try_from_slice(instruction_data).map_err(|e| {
        msg!("error parsing instruction: {}", e);
        ProgramError::InvalidInstructionData
    })?;

    match instruction {
        Instruction::CreateWallets => {
            msg!("creating wallets");
            process_create_wallets(program_id, accounts)?
        }
        Instruction::Init(ii) => {
            msg!("initializing account");
            process_init_state(program_id, accounts, ii)?;
        }
        Instruction::Deprecated1 => {
            msg!("deprecated");
        }
        Instruction::Deprecated2 => {
            msg!("deprecated");
        }
        Instruction::Deprecated3 => {
            msg!("deprecated");
        }
        Instruction::Deprecated4 => {
            msg!("deprecated");
        }
        Instruction::Vest => {
            msg!("vesting disabled");
            //process_vest(program_id, accounts)?;
        }
        Instruction::MigrateState => {
            msg!("migrating state");
            process_migrate_state(program_id, accounts)?;
        }
        Instruction::SetAdmin(SetAdminInstruction { admin }) => {
            msg!("updating admin state");
            process_set_admin(program_id, accounts, admin)?;
        }
        Instruction::DepositCommission => {
            msg!("depositing commission");
            process_deposit_commission(program_id, accounts)?;
        }
        Instruction::RegisterPost(args) => {
            msg!("registering post");
            process_register_post(program_id, accounts, args)?;
        }
        Instruction::Repost(args) => {
            msg!("reposting");
            process_repost(program_id, accounts, args)?;
        }
        Instruction::RedeemRepost => {
            msg!("redeem repost");
            process_redeem_repost(program_id, accounts)?;
        }
        Instruction::CreateRound(args) => {
            msg!("creating round");
            process_create_round(program_id, accounts, args)?;
        }
        Instruction::ClaimRoundVesting => {
            msg!("claiming round vesting");
            process_claim_round_vesting(program_id, accounts)?;
        }
    }

    Ok(())
}

#[macro_export]
macro_rules! find_keyed_address {
    ($program_id:expr, $seed:expr) => {
        $crate::find_keyed_address!($program_id, $seed, &[])
    };
    ($program_id:expr, $seed:expr, $token:expr) => {{
        let _: (&Pubkey, &[u8]) = ($program_id, $token);

        let seeds = &[V1, $seed, ($token)];
        let (addr, bump) = Pubkey::find_program_address(seeds, $program_id);

        (addr, &[V1, $seed, $token, &[bump]])
    }};
}

#[macro_export]
macro_rules! authority {
    ($program_id:expr) => {
        $crate::find_keyed_address!($program_id, AUTHORITY_SEED)
    };
}

#[macro_export]
macro_rules! contract_state {
    ($program_id:expr, $token:expr) => {
        $crate::find_keyed_address!($program_id, STATE_SEED, $token.as_ref())
    };
}

#[macro_export]
macro_rules! contract_wallet {
    ($program_id:expr, $token:expr) => {
        $crate::find_keyed_address!($program_id, WALLET_SEED, $token.as_ref())
    };
}

#[macro_export]
macro_rules! contract_vault {
    ($program_id:expr, $token:expr) => {
        $crate::find_keyed_address!($program_id, VAULT_SEED, $token.as_ref())
    };
}

#[macro_export]
macro_rules! contract_stash {
    ($program_id:expr, $token:expr) => {
        $crate::find_keyed_address!($program_id, STASH_SEED, $token.as_ref())
    };
}

#[macro_export]
macro_rules! master_post_mint {
    ($program_id:expr, $id:expr) => {
        $crate::find_keyed_address!($program_id, MASTER_POST_MINT_SEED, $id.as_ref())
    };
}

#[macro_export]
macro_rules! post_info {
    ($program_id:expr, $id:expr) => {
        $crate::find_keyed_address!($program_id, POST_INFO_SEED, $id.as_ref())
    };
}

#[macro_export]
macro_rules! repost_record {
    ($program_id:expr, $token:expr) => {
        $crate::find_keyed_address!($program_id, REPOST_RECORD_SEED, $token.as_ref())
    };
}

#[macro_export]
macro_rules! collection_mint {
    ($program_id:expr, $token:expr) => {
        $crate::find_keyed_address!($program_id, COLLECTION_MINT_SEED, $token.as_ref())
    };
}

// [] derived state account
// [] token mint addr
// [signer] funder
// [] token prog
// [] sysprog
fn process_init_state(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    args: InitInstruction,
) -> Result<(), ProgramError> {
    let account_info_iter = &mut accounts.iter();

    let state = next_account_info(account_info_iter)?;
    let mint_acc = next_account_info(account_info_iter)?;
    let fee_payer = next_account_info(account_info_iter)?;
    next_expected_account(account_info_iter, &spl_token::ID)?;
    next_expected_account(account_info_iter, &system_program::ID)?;

    let rent = Rent::get()?;
    let clock = Clock::get()?;

    if !spl_token::check_id(mint_acc.owner) {
        return Err(ProgramError::InvalidArgument);
    }

    let (state_addr, state_seed) = contract_state!(program_id, mint_acc.key);

    if *state.key != state_addr {
        msg!("invalid derived address {} != {}", state.key, state_addr);
        return Err(ProgramError::InvalidArgument);
    }

    // check token has no mint authority
    let mint = spl_token::state::Mint::unpack(&mint_acc.data.borrow())?;
    if mint.mint_authority != COption::None {
        msg!("token should have fixed supply");
        return Err(ProgramError::InvalidArgument);
    }

    msg!("creating state account");

    let lamports = rent.minimum_balance(STATE_ACC_SIZE);

    let create_instruction = system_instruction::create_account(
        fee_payer.key,
        &state_addr,
        lamports,
        STATE_ACC_SIZE as u64,
        program_id,
    );

    invoke_signed(&create_instruction, accounts, &[state_seed])?;

    if state.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }

    let mut data = state.try_borrow_mut_data()?;

    msg!("initializing state");
    init_state(&mut data, *mint_acc.key, args, clock.unix_timestamp)?;

    Ok(())
}

pub fn create_init_instruction(
    program_id: &Pubkey,
    state: &Pubkey,
    token: &Pubkey,
    feepayer: &Pubkey,
    args: InitInstruction,
) -> Result<instruction::Instruction, ProgramError> {
    let accounts = vec![
        AccountMeta::new(state.to_owned(), false),
        AccountMeta::new_readonly(token.to_owned(), false),
        AccountMeta::new_readonly(feepayer.to_owned(), true),
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(*program_id, false),
    ];

    Ok(instruction::Instruction {
        program_id: *program_id,
        accounts,
        data: Instruction::Init(args).try_to_vec()?,
    })
}

// [write] state
// [] swap state
// [] wsol account
pub fn process_migrate_state(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
) -> Result<(), ProgramError> {
    let account_info_iter = &mut accounts.iter();

    let state = next_account_info(account_info_iter)?;
    let token_mint = next_account_info(account_info_iter)?;
    let wsol_commission = next_account_info(account_info_iter)?;
    let treasury = next_account_info(account_info_iter)?;

    let mut state_data = state.data.borrow_mut();

    if state_data.is_empty() {
        return Err(ProgramError::InvalidAccountData);
    }

    try_migrate_state(
        &mut state_data,
        *token_mint.key,
        *wsol_commission.key,
        *treasury.key,
        Clock::get()?.unix_timestamp,
    )?;

    Ok(())
}

// [] token mint addr
// [write] derived wallet
// [write] derived vault wallet
// [signer] fee payer
// [] token prog
// [] sysprog
// [] rent var TODO fix weird local validator error
fn process_create_wallets(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
) -> Result<(), ProgramError> {
    let account_info_iter = &mut accounts.iter();

    let token_mint = next_account_info(account_info_iter)?;
    let wallet = next_account_info(account_info_iter)?;
    let vault = next_account_info(account_info_iter)?;
    let fee_payer = next_account_info(account_info_iter)?;

    let _ = next_account_info(account_info_iter)?;
    let _ = next_account_info(account_info_iter)?;
    let rent = Rent::get()?;

    let (wallet_addr, wallet_seed) = contract_wallet!(program_id, token_mint.key);
    if *wallet.key != wallet_addr {
        return Err(ProgramError::InvalidArgument);
    }

    let (vault_addr, vault_seed) = contract_vault!(program_id, token_mint.key);
    if *vault.key != vault_addr {
        return Err(ProgramError::InvalidArgument);
    }

    let (transfer_authority, _) = authority!(program_id);

    let acc_size = spl_token::state::Account::LEN;
    let min_balance = rent.minimum_balance(acc_size);

    let create_wallet = solana_program::system_instruction::create_account(
        fee_payer.key,
        &wallet_addr,
        min_balance,
        acc_size as u64,
        &spl_token::ID,
    );
    invoke_signed(&create_wallet, accounts, &[wallet_seed])?;

    let initialize_wallet = spl_token::instruction::initialize_account2(
        &spl_token::ID,
        &wallet_addr,
        token_mint.key,
        &transfer_authority,
    )?;
    invoke(&initialize_wallet, accounts)?;

    let create_vault = solana_program::system_instruction::create_account(
        fee_payer.key,
        &vault_addr,
        min_balance,
        acc_size as u64,
        &spl_token::ID,
    );
    invoke_signed(&create_vault, accounts, &[vault_seed])?;

    let initialize_vault = spl_token::instruction::initialize_account2(
        &spl_token::ID,
        &vault_addr,
        token_mint.key,
        &transfer_authority,
    )?;
    invoke(&initialize_vault, accounts)?;

    Ok(())
}

pub fn create_wallets(
    program_id: &Pubkey,
    token_mint: &Pubkey,
    fee_payer: &Pubkey,
) -> instruction::Instruction {
    let (wallet_addr, _) = contract_wallet!(program_id, token_mint);
    let (vault_addr, _) = contract_vault!(program_id, token_mint);

    let accounts = vec![
        AccountMeta::new_readonly(token_mint.to_owned(), false),
        AccountMeta::new(wallet_addr.to_owned(), false),
        AccountMeta::new(vault_addr.to_owned(), false),
        AccountMeta::new_readonly(fee_payer.to_owned(), true),
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(sysvar::rent::ID, false),
        AccountMeta::new_readonly(*program_id, false),
    ];

    instruction::Instruction {
        program_id: *program_id,
        accounts,
        data: Instruction::CreateWallets.try_to_vec().unwrap(),
    }
}

// [writable] state
// [signer] current admin
fn process_set_admin(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    new_admin: Pubkey,
) -> Result<(), ProgramError> {
    let account_info_iter = &mut accounts.iter();

    let (mut state, _) = next_entity::<_, ContractState>(account_info_iter, program_id)?;
    let _admin = next_signer_account(account_info_iter, &state.admin)?;

    state.admin = new_admin;

    Ok(())
}

pub mod swap_program {
    use solana_program::declare_id;

    declare_id!("SWPHMNgqcgHbZEa36JNXNNgbUD15yYLWp5uJUJktbGN");
}

pub mod old_program {
    use solana_program::declare_id;

    declare_id!("Human1nfyFpJsPU3BBKqWPwD9FeaZgdPYzDVrBj32Xj");
}

pub mod new_program {
    use solana_program::declare_id;

    declare_id!("Human1nfyFpJsPU3BBKqWPwD9FeaZgdPYzDVrBj32Xj");
}

// [writable] state
// [write] vault
// [write, sign] any account
// [] contract transfer authority
//
// [] swap state
// [] swap authority
// [write] swap token account (a)
// [write] swap wsol account (b)
//
// [write] pool mint
// [write] owner lp token addr
// [write] comission wSOL acc
// [sign] funder
// [] token program
// [] swap program
// [] sysprog
fn process_deposit_commission(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
) -> Result<(), ProgramError> {
    let account_info_iter = &mut accounts.iter();
    let rent = Rent::get()?;

    let (state, state_acc) = next_entity::<_, ContractState>(account_info_iter, program_id)?; // 1

    let (vault, _) = contract_vault!(program_id, state.token);
    let vault_wallet = next_expected_token_wallet(account_info_iter, &vault)?; // 2

    // any empty account
    let stash = next_account_info(account_info_iter)?; // 3

    let (authority, authority_seeds) = authority!(program_id);
    next_expected_account(account_info_iter, &authority)?; // 4

    let swap_state_acc = next_expected_account(account_info_iter, &state.swap_state)?; // 5
    let swap_state =
        spl_token_swap::state::SwapVersion::unpack(&swap_state_acc.try_borrow_data()?)?;

    let (swap_auth, _) =
        Pubkey::find_program_address(&[state.swap_state.as_ref()], &swap_program::ID);

    next_expected_account(account_info_iter, &swap_auth)?; // 6

    let swap_wallet_a = next_expected_account(account_info_iter, swap_state.token_a_account())?; // 7
    let swap_wallet_b = next_expected_account(account_info_iter, swap_state.token_b_account())?; // 8

    let pool_token_mint = next_expected_account(account_info_iter, swap_state.pool_mint())?; // 9

    let (owner_lp_wallet, _) =
        next_atoken_wallet(account_info_iter, &state.owner, swap_state.pool_mint())?; // 10

    let token_acc_size = spl_token::state::Account::LEN;
    let wallet_rent = rent.minimum_balance(token_acc_size);
    let state_rent = rent.minimum_balance(STATE_ACC_SIZE);

    // calculate deposit amount
    let to_deposit_lp = state_acc
        .lamports()
        .checked_sub(state_rent)
        .ok_or(Error::Overflow)?;

    let to_fund_wsol_account = to_deposit_lp
        .checked_add(wallet_rent)
        .ok_or(Error::Overflow)?;

    let pool_mint = spl_token::state::Mint::unpack(&pool_token_mint.try_borrow_data()?)?;
    let swap_token_wallet = spl_token::state::Account::unpack(&swap_wallet_a.try_borrow_data()?)?;
    let swap_wsol_wallet = spl_token::state::Account::unpack(&swap_wallet_b.try_borrow_data()?)?;

    let pool_tokens =
        calculate_pool_tokens(to_deposit_lp, pool_mint.supply, swap_wsol_wallet.amount)?;

    let calculated_tokens = spl_token_swap::curve::constant_product::pool_tokens_to_trading_tokens(
        pool_tokens as u128,
        pool_mint.supply as u128,
        swap_token_wallet.amount as u128,
        swap_wsol_wallet.amount as u128,
        spl_token_swap::curve::calculator::RoundDirection::Floor,
    )
    .ok_or(Error::Overflow)?;

    if calculated_tokens.token_a_amount > vault_wallet.amount.into() {
        msg!("lp deposit: insufficient tokens in vault");
        return Ok(());
    }

    // create wSOL acc
    {
        let mut state_lamports = state_acc.try_borrow_mut_lamports()?;
        let mut stash_lamports = stash.try_borrow_mut_lamports()?;

        **state_lamports = state_lamports.checked_sub(to_fund_wsol_account).unwrap();
        **stash_lamports = stash_lamports.checked_add(to_fund_wsol_account).unwrap();

        drop(state_lamports);
        drop(stash_lamports);

        let mut allocate = system_instruction::allocate(stash.key, token_acc_size as u64);
        // avoids UnbalancedInstruction error. Ask Timofey
        allocate
            .accounts
            .push(AccountMeta::new_readonly(*state_acc.key, false));

        invoke(&allocate, accounts)?;

        let assign = system_instruction::assign(stash.key, &spl_token::ID);
        invoke(&assign, accounts)?;

        let initialize = initialize_account2(
            &spl_token::ID,
            stash.key,
            &spl_token::native_mint::ID,
            &authority,
        )?;
        invoke(&initialize, accounts)?;
    }

    let (swap_authority, _) =
        Pubkey::find_program_address(&[state.swap_state.as_ref()], &swap_program::ID);

    // deposit
    let deposit = swap_instruction::deposit_all_token_types(
        &swap_program::ID,
        &spl_token::ID,
        &state.swap_state,
        &swap_authority,
        &authority,
        &vault,
        stash.key,
        swap_wallet_a.key,
        swap_wallet_b.key,
        pool_token_mint.key,
        &owner_lp_wallet,
        swap_instruction::DepositAllTokenTypes {
            pool_token_amount: pool_tokens,
            maximum_token_a_amount: u64::MAX, // we leave it because there is no slippage on chain
            maximum_token_b_amount: u64::MAX,
        },
    )?;
    invoke_signed(&deposit, accounts, &[authority_seeds])?;

    // close temp acc
    let close = close_account(&spl_token::ID, stash.key, state_acc.key, &authority, &[])?;
    invoke_signed(&close, accounts, &[authority_seeds])?;

    Ok(())
}

fn calculate_pool_tokens(
    wsol_amount: u64,
    pool_token_supply: u64,
    swap_token_balance: u64,
) -> Result<u64, ProgramError> {
    let wsol_amount = PreciseNumber::new(wsol_amount as u128).unwrap();
    let pool_token_supply = PreciseNumber::new(pool_token_supply as u128).unwrap();
    let swap_token_balance = PreciseNumber::new(swap_token_balance as u128).unwrap();

    Ok(wsol_amount
        .checked_mul(&pool_token_supply)
        .ok_or(Error::Overflow)?
        .checked_div(&swap_token_balance)
        .ok_or(Error::Overflow)?
        .floor()
        .ok_or(Error::Overflow)?
        .to_imprecise()
        .ok_or(Error::Overflow)?
        .try_into()
        .map_err(|_| Error::Overflow)?)
}

fn process_register_post(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    args: RegisterPostInstruction,
) -> Result<(), ProgramError> {
    let account_info_iter = &mut accounts.iter();

    let (authority, authority_seeds) = authority!(program_id);

    let (state, state_acc) = next_entity::<_, ContractState>(account_info_iter, program_id)?; // 1

    // check priviliged account signed this data
    // next_signer_account(account_info_iter, &state.admin)?;
    let _admin = next_account_info(account_info_iter)?;

    let payer = next_account_info(account_info_iter)?;

    let post_info = next_account_info(account_info_iter)?;

    let (master_post_mint, master_post_seeds) = master_post_mint!(program_id, &args.post_id);

    let master_post_mint_acc = next_expected_account(account_info_iter, &master_post_mint)?;

    let master_post_wallet =
        spl_associated_token_account::get_associated_token_address(&authority, &master_post_mint);
    next_expected_account(account_info_iter, &master_post_wallet)?;

    let (master_post_metadata_pda, _) =
        mpl_token_metadata::pda::find_metadata_account(&master_post_mint);

    next_expected_account(account_info_iter, &master_post_metadata_pda)?;

    let (master_edition_pda, _) =
        mpl_token_metadata::pda::find_master_edition_account(&master_post_mint);
    next_expected_account(account_info_iter, &master_edition_pda)?;

    next_expected_account(account_info_iter, &authority)?;

    let (collection_mint, collection_seeds) = collection_mint!(program_id, &state.token);

    let collection_mint_acc = next_expected_account(account_info_iter, &collection_mint)?;

    let collection_wallet =
        spl_associated_token_account::get_associated_token_address(&authority, &collection_mint);

    next_expected_account(account_info_iter, &collection_wallet)?;

    let (collection_metadata_pda, _) =
        mpl_token_metadata::pda::find_metadata_account(&collection_mint);

    let collection_metadata_acc =
        next_expected_account(account_info_iter, &collection_metadata_pda)?;

    let (collection_edition_pda, _) =
        mpl_token_metadata::pda::find_master_edition_account(&collection_mint);
    next_expected_account(account_info_iter, &collection_edition_pda)?;

    if !master_post_mint_acc.data_is_empty() {
        msg!("master mint already registered");
        return Ok(());
    }

    let creators = vec![
        Creator {
            address: args.royalty_addr,
            share: 100, // receives 100% of royalties
            verified: false,
        },
        Creator {
            // honoroble mention
            address: state.owner,
            share: 0,
            verified: false,
        },
        Creator {
            // for some reason update authority is also required
            address: authority,
            share: 0,
            verified: true,
        },
    ];

    if collection_mint_acc.data_is_empty() {
        // initialize collection
        mint_nft(
            &collection_mint,
            &authority,
            &authority,
            payer.key,
            accounts,
            &[collection_seeds.as_ref(), authority_seeds.as_ref()],
        )?;

        // change to v3 when mainnet finally rolls out
        let create_collection_metadata =
            mpl_token_metadata::instruction::create_metadata_accounts_v2(
                mpl_token_metadata::ID,
                collection_metadata_pda,
                collection_mint,
                authority,
                *payer.key,
                authority,
                args.collection_name,
                args.symbol.clone(),
                args.collection_metadata_uri,
                Some(creators.clone()),
                POST_ROYALTY_COMMISSION_BSP,
                true,
                true,
                None, // not a member of collection, but a collection itself
                None,
            );
        invoke_signed(&create_collection_metadata, accounts, &[authority_seeds])?;

        // create master edititon
        let make_master = mpl_token_metadata::instruction::create_master_edition_v3(
            mpl_token_metadata::ID,
            collection_edition_pda,
            collection_mint,
            authority,
            authority,
            collection_metadata_pda,
            *payer.key,
            Some(0), // collection must be unique nft
        );
        invoke_signed(&make_master, accounts, &[authority_seeds])?;
    }

    mint_nft(
        &master_post_mint,
        &authority,
        &authority,
        payer.key,
        accounts,
        &[master_post_seeds.as_ref(), authority_seeds.as_ref()],
    )?;

    // create metadata
    let create_metadata = mpl_token_metadata::instruction::create_metadata_accounts_v2(
        mpl_token_metadata::ID,
        master_post_metadata_pda,
        master_post_mint,
        authority,
        *payer.key,
        authority,
        args.post_name,
        args.symbol,
        args.post_metadata_uri,
        Some(creators),
        POST_ROYALTY_COMMISSION_BSP,
        true, // sign with update authority
        true, // mutable
        None,
        None, // no uses for this nft, lol
    );
    invoke_signed(&create_metadata, accounts, &[authority_seeds])?;

    // set as already sold
    let update_secondary = mpl_token_metadata::instruction::update_metadata_accounts_v2(
        mpl_token_metadata::ID,
        master_post_metadata_pda,
        authority,
        None,
        None,
        Some(true),
        None,
    );
    invoke_signed(&update_secondary, accounts, &[authority_seeds])?;

    // create master edititon
    let create_master = mpl_token_metadata::instruction::create_master_edition_v3(
        mpl_token_metadata::ID,
        master_edition_pda,
        master_post_mint,
        authority,
        authority,
        master_post_metadata_pda,
        *payer.key,
        None, // no printing limit
    );
    invoke_signed(&create_master, accounts, &[authority_seeds])?;

    // owner is checked inside
    let collection_meta: mpl_token_metadata::state::Metadata =
        mpl_token_metadata::state::Metadata::from_account_info(collection_metadata_acc)?;

    let verify_collection_f =
        if let Some(CollectionDetails::V1 { .. }) = collection_meta.collection_details {
            // we have 1.3.3 sized collection
            mpl_token_metadata::instruction::set_and_verify_sized_collection_item
        } else {
            // they still have this on mainnet
            mpl_token_metadata::instruction::set_and_verify_collection
        };

    let verify_collection = verify_collection_f(
        mpl_token_metadata::ID,
        master_post_metadata_pda,
        authority,
        *payer.key,
        authority,
        collection_mint,
        collection_metadata_pda,
        collection_edition_pda,
        None,
    );
    invoke_signed(&verify_collection, accounts, &[authority_seeds])?;

    // record creation date
    save_post_info(
        program_id,
        payer.key,
        state_acc.key,
        args.post_id,
        args.created_at,
        args.repost_price,
        post_info,
        accounts,
    )?;

    Ok(())
}

fn save_post_info(
    program_id: &Pubkey,
    payer_addr: &Pubkey,
    state_acc: &Pubkey,
    post_id: [u8; 32],
    created_at: i64,
    repost_price: u64,
    post_info: &AccountInfo,
    accounts: &[AccountInfo],
) -> ProgramResult {
    let (derived_info_key, post_info_seeds) = post_info!(program_id, post_id);

    if *post_info.key != derived_info_key {
        return Err(ProgramError::InvalidArgument);
    }

    let info = PostInfo {
        state: *state_acc,
        post_id,
        created_at,
        repost_price: Some(repost_price),
    };

    let rent = Rent::get()?;
    let lamports = rent.minimum_balance(PostInfo::SIZE);
    let create = system_instruction::create_account(
        payer_addr,
        &derived_info_key,
        lamports,
        PostInfo::SIZE as u64,
        program_id,
    );
    invoke_signed(&create, accounts, &[post_info_seeds])?;

    initialize_entity(info, post_info)?;

    Ok(())
}

// mints zero decimals token with supply of 1 to ata of `to_wallet`
fn mint_nft(
    mint: &Pubkey,
    to_wallet: &Pubkey,
    authority: &Pubkey,
    payer: &Pubkey,
    accounts: &[AccountInfo],
    seeds: &[&[&[u8]]],
) -> Result<(), ProgramError> {
    let rent = Rent::get()?;
    let size = spl_token::state::Mint::LEN;

    let create_mint = system_instruction::create_account(
        payer,
        mint,
        rent.minimum_balance(size),
        size as u64,
        &spl_token::ID,
    );
    invoke_signed(&create_mint, accounts, seeds)?;

    let initialize_mint = spl_token::instruction::initialize_mint2(
        &spl_token::ID,
        mint,
        authority,
        Some(authority), // apparently metaplex needs set freeze authority now
        0,               // zero decimals since it's an nft
    )?;
    invoke_signed(&initialize_mint, accounts, seeds)?;

    let atoken_wallet = spl_associated_token_account::get_associated_token_address(to_wallet, mint);

    let create_atoken = spl_associated_token_account::instruction::create_associated_token_account(
        payer,
        to_wallet,
        mint,
        &spl_token::ID,
    );
    invoke(&create_atoken, accounts)?;

    let mint =
        spl_token::instruction::mint_to(&spl_token::ID, mint, &atoken_wallet, authority, &[], 1)?;

    invoke_signed(&mint, accounts, seeds)?;

    Ok(())
}

fn process_repost(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    _args: RepostInstruction,
) -> Result<(), ProgramError> {
    let account_info_iter = &mut accounts.iter();
    let (authority, authority_seeds) = authority!(program_id);

    let (state, state_acc) = next_entity::<_, ContractState>(account_info_iter, program_id)?;

    let master_mint = next_account_info(account_info_iter)?;
    let master_wallet = next_account_info(account_info_iter)?;
    let master_metadata = next_account_info(account_info_iter)?;
    let master_edition = next_account_info(account_info_iter)?;

    let (post_info, _post_info_acc) = next_entity::<_, PostInfo>(account_info_iter, program_id)?;

    let repost_mint = next_account_info(account_info_iter)?;
    let repost_mint_key = repost_mint.key;

    let repost_metadata = next_account_info(account_info_iter)?;
    let repost_edition = next_account_info(account_info_iter)?;
    let _repost_edition_mark = next_account_info(account_info_iter)?;

    let user = next_account_info(account_info_iter)?;

    let _user_nft_wallet = next_account_info(account_info_iter)?;

    let derived_user_wallet = get_associated_token_address(user.key, &state.token);
    let user_wallet = next_expected_account(account_info_iter, &derived_user_wallet)?;

    next_expected_account(account_info_iter, &state.owner)?;
    next_expected_account(account_info_iter, &state.treasury_addr)?;

    let repost_record = next_account_info(account_info_iter)?;

    let authority = next_expected_account(account_info_iter, &authority)?;

    let swap_state_acc = next_expected_account(account_info_iter, &state.swap_state)?;
    let swap_state =
        spl_token_swap::state::SwapVersion::unpack(&swap_state_acc.try_borrow_data()?)?;

    let swap_wallet_token =
        next_expected_token_wallet(account_info_iter, swap_state.token_a_account())?;
    let swap_wallet_wsol =
        next_expected_token_wallet(account_info_iter, swap_state.token_b_account())?;

    // check this is the same post info
    if post_info.state != *state_acc.key {
        msg!("post belongs to a different state");
        return Err(ProgramError::InvalidArgument);
    }

    let (derived_master_mint, _) = master_post_mint!(program_id, post_info.post_id);

    if *master_mint.key != derived_master_mint {
        msg!("post_info id does not match master mint supplied");
        return Err(ProgramError::InvalidArgument);
    }

    let clock = Clock::get()?;

    if !post_info.can_repost(clock.unix_timestamp) {
        msg!("repost window expired");
        return Error::RepostWindowExpired.into();
    }

    let mut repost_price = post_info
        .repost_price
        .unwrap_or(DEFAULT_REPORT_PRICE_LAMPORTS);

    if *user.key == state.owner {
        // almost free repost for owner
        repost_price = 0;
    }

    msg!("repost price: {}", repost_price);

    let split = ContractState::calculate_split_by_lamports(repost_price).ok_or(Error::Overflow)?;

    invoke(
        &system_instruction::transfer(user.key, &state.owner, split.owner_split),
        accounts,
    )?;
    invoke(
        &system_instruction::transfer(user.key, &state.treasury_addr, split.treasury_split),
        accounts,
    )?;
    invoke(
        &system_instruction::transfer(user.key, state_acc.key, split.commission),
        accounts,
    )?;

    // mint fresh repost nft
    mint_nft(
        repost_mint_key,
        user.key,
        authority.key,
        user.key,
        accounts,
        &[authority_seeds],
    )?;

    if *master_edition.owner != mpl_token_metadata::ID {
        return Err(ProgramError::IllegalOwner);
    }

    let (expected_edition, _) =
        mpl_token_metadata::pda::find_master_edition_account(master_mint.key);

    if expected_edition != *master_edition.key {
        return Err(ProgramError::InvalidArgument);
    }

    let edition_data: mpl_token_metadata::state::MasterEditionV2 =
        mpl_token_metadata::state::MasterEditionV2::from_account_info(master_edition)?;

    let edition = edition_data.supply.checked_add(1).ok_or(Error::Overflow)?;

    let copy_from_master =
        mpl_token_metadata::instruction::mint_new_edition_from_master_edition_via_token(
            mpl_token_metadata::ID,
            *repost_metadata.key,
            *repost_edition.key,
            *master_edition.key,
            *repost_mint_key,
            *authority.key,
            *user.key,
            *authority.key,
            *master_wallet.key,
            *authority.key,
            *master_metadata.key,
            *master_mint.key,
            edition,
        );

    invoke_signed(&copy_from_master, accounts, &[authority_seeds])?;

    // set as already sold
    let update_secondary = mpl_token_metadata::instruction::update_metadata_accounts_v2(
        mpl_token_metadata::ID,
        *repost_metadata.key,
        *authority.key,
        None,
        None,
        Some(true),
        None,
    );
    invoke_signed(&update_secondary, accounts, &[authority_seeds])?;

    // fetch swap price
    let amount = calculate_tokens_for_repost_fee(
        repost_price,
        swap_wallet_wsol.amount,
        swap_wallet_token.amount,
    )
    .ok_or(Error::Overflow)?
    .max(1);

    // record repost
    let repost = RepostRecord {
        state: *state_acc.key,
        token: state.token,
        user: *user.key,
        post_id: post_info.post_id,
        reposted_at: clock.unix_timestamp,
        receive_amount: amount,
    };

    save_repost_record(
        program_id,
        user.key,
        repost,
        repost_record,
        repost_mint_key,
        accounts,
    )?;

    if user_wallet.data_is_empty() {
        // also create user a atoken wallet for later distribution

        let create_atoken =
            spl_associated_token_account::instruction::create_associated_token_account(
                user.key,
                user.key,
                &state.token,
                &spl_token::ID,
            );
        invoke(&create_atoken, accounts)?;
    }

    msg!("will-receive {}", amount);

    Ok(())
}

fn calculate_tokens_for_repost_fee(
    amount_in: u64,
    wsol_amount: u64,
    chatlans_amount: u64,
) -> Option<u64> {
    // amountOut := (amountB * (amountA + amountIn) - constProd) / (amountA + amountIn)
    let amount_in = PreciseNumber::new(amount_in as u128)?;

    // multiply both sides of liqudity by some large number for precise calculation
    // (we are only interested in ratio)
    let coeff = PreciseNumber::new(1e9 as u128)?;

    let a_balance = PreciseNumber::new(wsol_amount as u128)?.checked_mul(&coeff)?;
    let b_balance = PreciseNumber::new(chatlans_amount as u128)?.checked_mul(&coeff)?;

    let const_prod = a_balance.checked_mul(&b_balance)?;

    let a = a_balance.checked_add(&amount_in)?;

    let y = b_balance.checked_mul(&a)?.checked_sub(&const_prod)?;

    let amount_out = y.checked_div(&a)?;

    u64::try_from(amount_out.to_imprecise()?).ok()
}

fn save_repost_record(
    program_id: &Pubkey,
    payer_addr: &Pubkey,
    record: RepostRecord,
    record_acc: &AccountInfo,
    repost_mint: &Pubkey,
    accounts: &[AccountInfo],
) -> ProgramResult {
    let (_, repost_record_seeds) = repost_record!(program_id, repost_mint);

    let rent = Rent::get()?;
    let lamports = rent.minimum_balance(RepostRecord::SIZE);
    let create = system_instruction::create_account(
        payer_addr,
        record_acc.key,
        lamports,
        RepostRecord::SIZE as u64,
        program_id,
    );
    invoke_signed(&create, accounts, &[repost_record_seeds])?;

    initialize_entity(record, record_acc)?;

    Ok(())
}

// claim RepostRecord
fn process_redeem_repost(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
) -> Result<(), ProgramError> {
    let account_info_iter = &mut accounts.iter();

    let (state, state_acc) = next_entity::<_, ContractState>(account_info_iter, program_id)?; // 1

    let (record, record_acc) = next_entity::<_, RepostRecord>(account_info_iter, program_id)?; // 2

    let (vault_addr, _) = contract_vault!(program_id, state.token);
    let _vault_wallet = next_expected_token_wallet(account_info_iter, &vault_addr)?; // 3

    let user = next_expected_account(account_info_iter, &record.user)?; // 4
    let (user_wallet_addr, _user_wallet) =
        next_atoken_wallet(account_info_iter, &record.user, &record.token)?; // 5

    let (authority, authority_seeds) = authority!(program_id);
    next_expected_account(account_info_iter, &authority)?; // 6

    if record.state != *state_acc.key {
        msg!("state != record.state");
        return Err(ProgramError::InvalidArgument);
    }

    let clock = Clock::get()?;

    let can_redeem = record.can_redeem(clock.unix_timestamp);
    msg!("can redeem: {}", can_redeem);

    // check if eligible
    if !can_redeem {
        msg!("cant-redeem-now");
        return Ok(());
    }

    let transfer = spl_token::instruction::transfer(
        &spl_token::ID,
        &vault_addr,
        &user_wallet_addr,
        &authority,
        &[],
        record.receive_amount,
    )?;

    invoke_signed(&transfer, accounts, &[authority_seeds])?;

    drop(record);

    // return lamports to the user
    let mut user_lamports = user.try_borrow_mut_lamports()?;

    erase_repost_record(record_acc, &mut user_lamports)?;

    Ok(())
}

fn erase_repost_record(acc: &AccountInfo, payer_lamports: &mut u64) -> Result<(), ProgramError> {
    // withdraw all lamports from state
    *payer_lamports = payer_lamports
        .checked_add(mem::take(*acc.try_borrow_mut_lamports()?))
        .ok_or(Error::Overflow)?;

    let mut data = acc.try_borrow_mut_data()?;
    let l = data.len();
    sol_memset(&mut data, 0, l);

    Ok(())
}

fn process_create_round(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    args: CreateRoundInstruction,
) -> Result<(), ProgramError> {
    let account_info_iter = &mut accounts.iter();

    let (mut state, state_acc) =
        next_entity::<_, ContractState>(account_info_iter, program_id).unwrap(); // 1

    let (vault_addr, _) = contract_vault!(program_id, state.token);
    let _vault_wallet = next_expected_token_wallet(account_info_iter, &vault_addr)?; // 2

    let (authority, authority_seeds) = authority!(program_id);
    next_expected_account(account_info_iter, &authority)?; // 3

    let fanout_acc = next_account_info(account_info_iter)?; // 4
    let payer = next_account_info(account_info_iter)?; // 5

    // passed as is

    let round = next_account_info(account_info_iter)?; // 6
    let offer_wallet = next_account_info(account_info_iter)?; // 7
    let offer_mint = next_account_info(account_info_iter)?; // 8

    let bid_wallet = next_account_info(account_info_iter)?; // 9
    let bid_mint = next_account_info(account_info_iter)?; // 10

    let round_authority = next_account_info(account_info_iter)?; // 11

    //

    let expected_members = vec![
        fanout::state::Member {
            address: state.treasury_addr, // treasury addr
            share: 9650,
        },
        fanout::state::Member {
            address: state.admin, // our fee
            share: 250,
        },
        fanout::state::Member {
            address: *state_acc.key, // LP
            share: 100,
        },
    ];

    if *fanout_acc.owner == fanout::ID {
        let fanout_data = fanout_acc.try_borrow_data()?;
        let fanout = fanout::state::Fanout::try_deserialize(&mut fanout_data.as_ref())?;

        if fanout.members != expected_members {
            msg!("fanout.members != expected_members");
            return Err(ProgramError::InvalidArgument);
        }
    } else {
        let rent = Rent::get()?;

        let size = 8 + 8 + 4 + (32 + 2) * expected_members.len();

        // system create
        let create_ix = system_instruction::create_account(
            payer.key,
            fanout_acc.key,
            rent.minimum_balance(size),
            size as u64,
            &fanout::ID,
        );

        invoke(&create_ix, accounts)?;

        // initalize fanout
        let data = fanout::instruction::Initialize {
            members: expected_members,
        }
        .data();

        let init_ix = solana_program::instruction::Instruction {
            program_id: fanout::ID,
            accounts: vec![AccountMeta::new(*fanout_acc.key, false)],
            data,
        };

        msg!("initializing fanout acc");
        invoke(&init_ix, accounts)?;
    }

    // amount is chosen by the user
    // todo: limits on how much one can offer
    let amount = args.offer_amount;

    // approve this amount to be tranfered from the vault
    let approve_ix = spl_token::instruction::approve(
        &spl_token::ID,
        &vault_addr,
        &authority,
        &authority,
        &[],
        amount,
    )?;
    invoke_signed(&approve_ix, accounts, &[authority_seeds])?;

    let data = round::instruction::CreateRound {
        params: round::CreateRoundParams {
            heir: state.owner,
            recipient: *fanout_acc.key,
            target_bid: args.target_bid,
            bidding_start: args.bidding_start,
            bidding_end: args.bidding_end,
        },
    }
    .data();

    let ix = solana_program::instruction::Instruction {
        program_id: round::ID,
        accounts: vec![
            AccountMeta::new(*round.key, true),
            AccountMeta::new(*offer_wallet.key, false),
            AccountMeta::new_readonly(*offer_mint.key, false),
            AccountMeta::new(vault_addr, false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(*bid_wallet.key, false),
            AccountMeta::new_readonly(*bid_mint.key, false),
            AccountMeta::new_readonly(*round_authority.key, false),
            AccountMeta::new(*payer.key, true),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(solana_program::system_program::ID, false),
        ],
        data,
    };
    invoke_signed(&ix, accounts, &[authority_seeds])?;

    state.current_round = Some(*round.key);

    Ok(())
}

fn process_claim_round_vesting(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
) -> Result<(), ProgramError> {
    let account_info_iter = &mut accounts.iter();

    let (mut state, _state_acc) = next_entity::<_, ContractState>(account_info_iter, program_id)?; // 1

    let (vault_addr, _) = contract_vault!(program_id, state.token);
    let vault_wallet = next_expected_token_wallet(account_info_iter, &vault_addr)?; // 2

    let (authority, authority_seeds) = authority!(program_id);
    next_expected_account(account_info_iter, &authority)?; // 3

    let round_acc = next_account_info(account_info_iter)?; // 4

    let (owner_wallet, _) = next_atoken_wallet(account_info_iter, &state.owner, &state.token)?; // 5

    // deserialize round
    let round_data = round_acc.try_borrow_data()?;
    let round = round::state::Round::try_deserialize(&mut round_data.as_ref())?;

    // check round is saved in state
    if state.current_round != Some(*round_acc.key) {
        msg!("state.current_round != round_acc.key");
        return Err(ProgramError::InvalidArgument);
    }

    // check round was accepted
    if round.status != round::state::RoundStatus::Accepted {
        msg!("round.status != RoundStatus::Accepted");
        return Err(ProgramError::InvalidArgument);
    }

    let amount = round
        .total_offer
        .expect("accepted round should always have total_offer set")
        .checked_div(10)
        .unwrap();

    // check
    let amount = amount.min(vault_wallet.amount);

    // tranfer 10% of round offer to owner
    let ix = spl_token::instruction::transfer(
        &spl_token::ID,
        &vault_addr,
        &owner_wallet,
        &authority,
        &[],
        amount,
    )?;

    invoke_signed(&ix, accounts, &[authority_seeds])?;

    state.current_round = None;
    state.completed_rounds_count = state.completed_rounds_count.checked_add(1).unwrap();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_repost_fee() {
        let x = calculate_tokens_for_repost_fee(DEFAULT_REPORT_PRICE_LAMPORTS, 10000, 1).unwrap();
        assert_eq!(x, 1000);
        // ratio maintained
        let x = calculate_tokens_for_repost_fee(DEFAULT_REPORT_PRICE_LAMPORTS, 2000010000, 200001)
            .unwrap();
        assert_eq!(x, 1000);
    }
}
