#![forbid(unsafe_code)]
#![cfg(not(feature = "no-entrypoint"))]
#![deny(clippy::all)]
#![deny(clippy::integer_arithmetic)]
#![allow(clippy::too_many_arguments)]

pub mod error;

use error::Error;

pub mod state;
use human_common::{
    entity::{entity_from_acc, initialize_entity, next_entity, Entity},
    utils::{next_expected_account, next_signer_account},
};
use spl_associated_token_account::get_associated_token_address;
use spl_governance::state::token_owner_record;
use state::{Distribution, State, Voucher};

pub mod instruction;
use instruction::{InitializeArgs, RoyaltyInstruction};

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint,
    entrypoint::ProgramResult,
    log::sol_log,
    msg,
    native_token::LAMPORTS_PER_SOL,
    program::invoke_signed,
    program_error::ProgramError,
    program_memory::sol_memcpy,
    program_pack::Pack,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction::{self, create_account},
    system_program,
    sysvar::{
        instructions::{self},
        Sysvar,
    },
};

use borsh::BorshSerialize;
use spl_governance_addin_api::{
    max_voter_weight::MaxVoterWeightRecord, voter_weight::VoterWeightRecord,
};
use spl_token::state as token_state;

use human_common::utils::{next_atoken_wallet, next_expected_token_wallet};

entrypoint!(process_instruction);
pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    use borsh::BorshDeserialize;
    use RoyaltyInstruction::*;

    let instruction = RoyaltyInstruction::try_from_slice(instruction_data).map_err(|e| {
        msg!("error parsing instruction: {}", e);
        ProgramError::InvalidInstructionData
    })?;

    let result = match instruction {
        Initialize(args) => {
            msg!("initializing royalty state");
            process_initialize(program_id, accounts, args)
        }
        Distribute => {
            msg!("distributing balance");
            process_distribute(program_id, accounts)
        }
        Claim => {
            msg!("withdrawing reward");
            process_claim(program_id, accounts)
        }
        DepositTokens(amount) => {
            msg!("depositing tokens");
            process_deposit(program_id, accounts, amount)
        }
        WithdrawTokens(amount) => {
            msg!("withdrawing tokens");
            process_withdraw(program_id, accounts, amount)
        }
        SyncWeightRecord => {
            msg!("syncing weight record");
            process_sync_weight_record(program_id, accounts)
        }
        Migrate(args) => {
            msg!("migrating state");
            process_migrate(program_id, accounts, &args.realm_addr, &args.vault_addr)
        }
    };

    if let Err(ref e) = result {
        sol_log(&e.to_string());
    }

    result
}

// [write] state account assigned to this program with enough size and
// [] token mint
// [write] derived wallet
// [write, signer] feepayer
// [] sysprog
// [] token prog
fn process_initialize(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    args: InitializeArgs,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();

    let state_acc = next_account_info(account_info_iter)?;
    let mint = next_account_info(account_info_iter)?;
    let wallet = next_account_info(account_info_iter)?;
    let fee_payer = next_account_info(account_info_iter)?;
    let _sysprog = next_account_info(account_info_iter)?;
    let _token_prog = next_account_info(account_info_iter)?;
    let rent = Rent::get()?;

    if state_acc.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }

    if !rent.is_exempt(state_acc.lamports(), state_acc.data_len()) {
        return Err(ProgramError::AccountNotRentExempt);
    }

    let (derived_wallet, wallet_seed) = wallet!(program_id, state_acc.key);

    if *wallet.key != derived_wallet {
        return Err(ProgramError::InvalidArgument);
    }

    if token_state::Account::unpack(&wallet.try_borrow_data()?).is_ok() {
        return Err(ProgramError::AccountAlreadyInitialized);
    }

    let size = token_state::Account::LEN;

    let create = system_instruction::create_account(
        fee_payer.key,
        &derived_wallet,
        rent.minimum_balance(size),
        size as u64,
        &spl_token::ID,
    );
    invoke_signed(&create, accounts, &[&wallet_seed])?;

    let initialize = spl_token::instruction::initialize_account2(
        &spl_token::ID,
        &derived_wallet,
        mint.key,
        wallet.key,
    )?;
    invoke_signed(&initialize, accounts, &[&wallet_seed])?;

    if !args.settings.valid() {
        return Err(ProgramError::InvalidArgument);
    }

    let state = State {
        wallet: derived_wallet,
        owner: args.owner,
        host: args.host,
        settings: args.settings,
        drop_idx: 0,
        total_distributed: 0,
        total_user_distributed: 0,
        vouchers_count: 0,
        tokens_held: 0,
        distribution: None,
        realm_addr: args.realm_addr,
        vault_addr: args.vault_addr,
        token_mint: *mint.key,
    };

    initialize_entity(state, state_acc)?;

    Ok(())
}

// [write] state
// [write] state wallet
// [write] derived voucher
// [sign] user
// [write] user atoken wallet
// [] vault address
// [] owner atoken address
// [write] derived max voter record
// [write, signer] funder
// [] sysprog
// [] tokenprog
// [] instructions var
fn process_deposit(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();

    let (mut state, state_acc) = next_entity::<_, State>(account_info_iter, program_id)?;
    let wallet = next_expected_token_wallet(account_info_iter, &state.wallet)?;
    let voucher_acc = next_account_info(account_info_iter)?;
    let user = next_account_info(account_info_iter)?;
    let (user_wallet_addr, _user_wallet) =
        next_atoken_wallet(account_info_iter, user.key, &wallet.mint)?;

    let vault = next_expected_token_wallet(account_info_iter, &state.vault_addr)?;
    let owner_atoken_addr = get_associated_token_address(&state.owner, &state.token_mint);
    let owner_atoken = next_expected_token_wallet(account_info_iter, &owner_atoken_addr)?;

    let max_weight_record = next_account_info(account_info_iter)?;
    let funder = next_account_info(account_info_iter)?;
    next_expected_account(account_info_iter, &system_program::ID)?;
    next_expected_account(account_info_iter, &spl_token::ID)?;

    let instructions = next_expected_account(account_info_iter, &instructions::ID)?;
    let rent = Rent::get()?;

    check_no_other_programs(instructions, &[program_id, &system_program::ID])?;

    // quick hack to avoid migration :p
    state.settings.host_fee = 250;
    state.settings.host_flat_fee = 0;
    state.settings.owner_fee = 0;

    if !user.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    if *user.key == state.owner || *user.key == state.host {
        msg!("host or owner distribution is controlled separately");
        return Error::BlackistedForEnroll.into();
    }

    let (voucher_addr, voucher_seeds) = voucher!(program_id, state_acc.key, user.key);
    if *voucher_acc.key != voucher_addr {
        msg!("invalid derived voucher seeds");
        return Err(ProgramError::InvalidArgument);
    }

    if state.distribution.is_some() {
        return Error::TemporaryUnavailable.into();
    }

    if !Voucher::is_initialized(&voucher_acc.try_borrow_data()?) {
        if amount < state.settings.min_token_to_enroll {
            return Error::LessThanDepositMinimum.into();
        }

        let rent_minimum = rent.minimum_balance(Voucher::SIZE);

        let create = system_instruction::create_account(
            funder.key,
            &voucher_addr,
            rent_minimum,
            Voucher::SIZE as u64,
            program_id,
        );

        invoke_signed(&create, accounts, &[&voucher_seeds])?;

        let voucher = Voucher {
            user: *user.key,
            state: *state_acc.key,
            balance: 0,
            drop_idx: state.drop_idx,
        };

        state.vouchers_count = state.vouchers_count.checked_add(1).ok_or(Error::Overflow)?;

        initialize_entity(voucher, voucher_acc)?;
    }

    let mut voucher = entity_from_acc::<Voucher>(voucher_acc, program_id)?;

    let (wallet_addr, wallet_seed) = wallet!(program_id, state_acc.key);

    let transfer = spl_token::instruction::transfer(
        &spl_token::ID,
        &user_wallet_addr,
        &wallet_addr,
        user.key,
        &[],
        amount,
    )?;

    invoke_signed(&transfer, accounts, &[&wallet_seed])?;

    voucher.balance = voucher.balance.checked_add(amount).ok_or(Error::Overflow)?;

    state.tokens_held = state
        .tokens_held
        .checked_add(amount)
        .ok_or(Error::Overflow)?;

    let max_vote_weight = state
        .tokens_held
        .checked_add(vault.amount)
        .ok_or(Error::Overflow)?
        .checked_add(owner_atoken.amount)
        .ok_or(Error::Overflow)?;

    update_max_voter_weight(
        program_id,
        state_acc.key,
        &state.realm_addr,
        &wallet.mint,
        max_vote_weight,
        max_weight_record,
        funder,
    )?;

    Ok(())
}

// [write] state
// [write] state wallet
// [write] voucher
// [sign] user
// [write] user atoken wallet
// [] voter record owner by governance program with no outstanding votes
// [writable] derived vote weight record
// [] vault wallet
// [] owner atoken wallet
// [writable] derived max vote weight
// [sign] funder
// [] token prog
// [] sysprog
fn process_withdraw(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();

    let (mut state, state_acc) = next_entity::<_, State>(account_info_iter, program_id)?;
    let wallet = next_expected_token_wallet(account_info_iter, &state.wallet)?;
    let (mut voucher, _voucher_acc) = next_entity::<_, Voucher>(account_info_iter, program_id)?;

    next_signer_account(account_info_iter, &voucher.user)?;
    let (user_wallet_addr, _user_wallet) =
        next_atoken_wallet(account_info_iter, &voucher.user, &wallet.mint)?;

    let voter_record = next_account_info(account_info_iter)?; // TODO
    let vote_weight_record = next_account_info(account_info_iter)?;

    let vault = next_expected_token_wallet(account_info_iter, &state.vault_addr)?;
    let owner_atoken_addr = get_associated_token_address(&state.owner, &state.token_mint);
    let owner_atoken = next_expected_token_wallet(account_info_iter, &owner_atoken_addr)?;

    let max_weight_record = next_account_info(account_info_iter)?;
    let funder = next_account_info(account_info_iter)?;

    next_expected_account(account_info_iter, &spl_token::ID)?;
    next_expected_account(account_info_iter, &system_program::ID)?;

    assert_no_unrequilished_votes(
        voter_record,
        &state.realm_addr,
        &state.token_mint,
        &voucher.user,
    )?;

    if state.distribution.is_some() {
        return Error::TemporaryUnavailable.into();
    }

    if voucher.balance < amount {
        return Error::InsufficientBalance.into();
    }

    let after_balance = voucher.balance.checked_sub(amount).unwrap();

    if after_balance != 0 && after_balance < state.settings.min_token_to_enroll {
        return Error::LessThanDepositMinimum.into();
    }

    let transfer = spl_token::instruction::transfer(
        &spl_token::ID,
        &state.wallet,
        &user_wallet_addr,
        &state.wallet,
        &[],
        amount,
    )?;

    let (_, wallet_seed) = wallet!(program_id, state_acc.key);
    invoke_signed(&transfer, accounts, &[&wallet_seed])?;

    voucher.balance = voucher.balance.checked_sub(amount).ok_or(Error::Overflow)?;
    state.tokens_held = state
        .tokens_held
        .checked_sub(amount)
        .ok_or(Error::Overflow)?;

    let max_vote_weight = state
        .tokens_held
        .checked_add(vault.amount)
        .ok_or(Error::Overflow)?
        .checked_add(owner_atoken.amount)
        .ok_or(Error::Overflow)?;

    update_voter_weight(
        program_id,
        state_acc.key,
        &state.realm_addr,
        &wallet.mint,
        &voucher.user,
        voucher.balance,
        vote_weight_record,
        funder,
    )?;

    update_max_voter_weight(
        program_id,
        state_acc.key,
        &state.realm_addr,
        &wallet.mint,
        max_vote_weight,
        max_weight_record,
        funder,
    )?;

    Ok(())
}

pub mod governance_program {
    use solana_program::declare_id;

    declare_id!("hmndaoPYAPUbgmABeMCQom7poo3QMLooYbinzhXE1j7");
}

fn assert_no_unrequilished_votes(
    acc: &AccountInfo,
    realm_addr: &Pubkey,
    token_mint: &Pubkey,
    user: &Pubkey,
) -> ProgramResult {
    let expected_addr = token_owner_record::get_token_owner_record_address(
        &governance_program::ID,
        realm_addr,
        token_mint,
        user,
    );

    if expected_addr != *acc.key {
        msg!(
            "invalid token_owner_record address: expected {} but got {}",
            expected_addr,
            acc.key
        );
        return Err(ProgramError::InvalidArgument);
    }

    // since we already checked this account's address is derived from user addr
    // and governance program, we can safely assume that when record is not initialized,
    // user is guaranteed to have no outstanding votes
    if acc.data_is_empty() {
        return Ok(());
    }

    let token_owner_record =
        token_owner_record::get_token_owner_record_data(&governance_program::ID, acc)?;

    if token_owner_record.realm != *realm_addr {
        return Err(ProgramError::InvalidArgument);
    }

    if token_owner_record.governing_token_mint != *token_mint {
        return Err(ProgramError::InvalidArgument);
    }

    if token_owner_record.governing_token_owner != *user {
        return Err(ProgramError::InvalidArgument);
    }

    if token_owner_record.unrelinquished_votes_count > 0 {
        return Error::UnrelinquishedVotes.into();
    }

    Ok(())
}

const DISTRIBUTION_MINIMUM_LAMPORTS: u64 = LAMPORTS_PER_SOL / 2; // 0.5 SOL

fn check_no_other_programs(acc: &AccountInfo, allowed_programs: &[&Pubkey]) -> ProgramResult {
    if !instructions::check_id(acc.key) {
        return Err(ProgramError::UnsupportedSysvar);
    }

    let mut count_buf = [0u8; 2];
    count_buf.copy_from_slice(&acc.try_borrow_data()?[..2]);
    let count = u16::from_le_bytes(count_buf);

    for i in 0..count {
        let inst = instructions::load_instruction_at_checked(i as usize, acc)?;

        if !allowed_programs.iter().any(|id| **id == inst.program_id) {
            return Error::NoOtherProgramsAllowed.into();
        }
    }

    Ok(())
}

// [write] state account
// [write] owner acc
// [write] host acc
// [] instructions var
// for n:
//  [write] voucher
fn process_distribute(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter().peekable();

    let (mut state, state_acc) = next_entity::<_, State>(account_info_iter, program_id)?;
    let owner = next_expected_account(account_info_iter, &state.owner)?;
    let host = next_expected_account(account_info_iter, &state.host)?;
    let instructions = next_expected_account(account_info_iter, &instructions::ID)?;

    check_no_other_programs(instructions, &[program_id])?;

    let rent = Rent::get()?;

    let mut state_lamports = state_acc.try_borrow_mut_lamports()?;

    if state.distribution.is_none() {
        msg!("starting distribution");
        // before starting distribution:
        // 1. check if there is enough balance
        // 2. assure there is least some tokens
        // 3. calculate owner and host fees and deduct them from distribution
        let minimum_balance = rent.minimum_balance(State::SIZE);

        let excess_balance = state_lamports
            .checked_sub(minimum_balance)
            .ok_or(ProgramError::AccountNotRentExempt)?;

        if excess_balance < DISTRIBUTION_MINIMUM_LAMPORTS {
            msg!(
                "insufficient lamports to start distribution. need: {} have: {}",
                DISTRIBUTION_MINIMUM_LAMPORTS,
                excess_balance
            );
            return Error::NotEnoughToStartDistribution.into();
        }

        // just to be sure we don't give more than we have after rounding
        let amount_to_distribute = excess_balance.checked_sub(1).unwrap();

        let split = state
            .settings
            .calculate_split(amount_to_distribute, state.vouchers_count)
            .ok_or(Error::Overflow)?;

        {
            let mut owner_lamports = owner.try_borrow_mut_lamports()?;

            **state_lamports = state_lamports
                .checked_sub(split.owner_comission)
                .ok_or(Error::Overflow)?;

            **owner_lamports = owner_lamports
                .checked_add(split.owner_comission)
                .ok_or(Error::Overflow)?;
        }

        {
            let mut host_lamports = host.try_borrow_mut_lamports()?;

            **state_lamports = state_lamports
                .checked_sub(split.host_comission)
                .ok_or(Error::Overflow)?;

            **host_lamports = host_lamports
                .checked_add(split.host_comission)
                .ok_or(Error::Overflow)?;
        }

        state.total_distributed = state
            .total_distributed
            .checked_add(excess_balance)
            .ok_or(Error::Overflow)?;

        state.total_user_distributed = state
            .total_user_distributed
            .checked_add(split.distribute_amount)
            .ok_or(Error::Overflow)?;

        state.distribution = Some(Distribution {
            distribute_amount: split.distribute_amount,
            seen_vouchers: 0,
        })
    }

    msg!("event-token");
    state.token_mint.log();

    let mut dist_state = state.distribution.as_ref().cloned().unwrap();

    while account_info_iter.peek().is_some() {
        let (mut voucher, voucher_acc) = next_entity::<_, Voucher>(account_info_iter, program_id)?;

        if voucher.state != *state_acc.key {
            return Error::InvalidVoucher.into();
        }

        msg!("event-voucher-user");
        voucher.user.log();

        // apply voucher
        let to_send = dist_state.distribute_to(&mut voucher, state.drop_idx, state.tokens_held)?;

        let mut voucher_lamports = voucher_acc.try_borrow_mut_lamports()?;

        **state_lamports = state_lamports.checked_sub(to_send).ok_or(Error::Overflow)?;
        **voucher_lamports = voucher_lamports
            .checked_add(to_send)
            .ok_or(Error::Overflow)?;
    }

    // sanity check. could be removed when feature is activated on mainnet
    assert!(
        rent.is_exempt(**state_lamports, State::SIZE),
        "very bad: state account would become not rent-exempt"
    );

    if dist_state.seen_vouchers == state.vouchers_count {
        msg!("distribution completed");

        msg!("event-distributed");
        msg!("{}", dist_state.distribute_amount);

        state.drop_idx = state.drop_idx.checked_add(1).ok_or(Error::Overflow)?;
        state.distribution = None;
        return Ok(());
    }

    state.distribution = Some(dist_state);

    Ok(())
}

// [write] voucher
// [signer, writer] user
fn process_claim(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter().peekable();

    let (voucher, voucher_acc) = next_entity::<_, Voucher>(account_info_iter, program_id)?;
    let user = next_signer_account(account_info_iter, &voucher.user)?;
    let rent = Rent::get()?;

    let rent_minimum = rent.minimum_balance(Voucher::SIZE);

    let mut voucher_lamports = voucher_acc.try_borrow_mut_lamports()?;
    let mut user_lamports = user.try_borrow_mut_lamports()?;

    let withdraw_amount = voucher_lamports
        .checked_sub(rent_minimum)
        .ok_or(ProgramError::AccountNotRentExempt)?;

    **voucher_lamports = voucher_lamports
        .checked_sub(withdraw_amount)
        .ok_or(Error::Overflow)?;

    **user_lamports = user_lamports
        .checked_add(withdraw_amount)
        .ok_or(Error::Overflow)?;

    Ok(())
}

// [write] state
// [] user
// [write] user voucher
// [write] derived voter record

// [] owner's vault
// [] owner atoken wallet
// [write] derived max voter record

// [signer] funder
// [] sysprog
fn process_sync_weight_record(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let (state, state_acc) = next_entity::<_, State>(account_info_iter, program_id)?;

    let user = next_account_info(account_info_iter)?;
    let voucher_acc = next_account_info(account_info_iter)?;
    let record_acc = next_account_info(account_info_iter)?;

    let vault = next_expected_token_wallet(account_info_iter, &state.vault_addr)?;
    let atoken_addr = get_associated_token_address(&state.owner, &state.token_mint);
    let owner_atoken = next_expected_token_wallet(account_info_iter, &atoken_addr)?;
    let max_record_acc = next_account_info(account_info_iter)?;

    let funder = next_account_info(account_info_iter)?;
    let _sysprog = next_account_info(account_info_iter)?;

    let balance = if *user.key == state.owner {
        owner_atoken
            .amount
            .checked_add(vault.amount)
            .ok_or(Error::Overflow)?
    } else {
        let voucher = entity_from_acc::<Voucher>(voucher_acc, program_id)?;

        if voucher.user != *user.key {
            msg!("voucher belongs to another user");
            return Err(ProgramError::InvalidArgument);
        }

        voucher.balance
    };

    update_voter_weight(
        program_id,
        state_acc.key,
        &state.realm_addr,
        &state.token_mint,
        user.key,
        balance,
        record_acc,
        funder,
    )?;

    let max_vote_weight = state
        .tokens_held
        .checked_add(vault.amount)
        .ok_or(Error::Overflow)?
        .checked_add(owner_atoken.amount)
        .ok_or(Error::Overflow)?;

    update_max_voter_weight(
        program_id,
        state_acc.key,
        &state.realm_addr,
        &state.token_mint,
        max_vote_weight,
        max_record_acc,
        funder,
    )?;

    Ok(())
}

fn update_voter_weight<'a, 'b>(
    program_id: &Pubkey,
    state_addr: &Pubkey,
    realm_addr: &Pubkey,
    realm_token: &Pubkey,
    user: &Pubkey,
    balance: u64,
    record_acc: &'a AccountInfo<'b>,
    funder: &'a AccountInfo<'b>,
) -> ProgramResult {
    let record = VoterWeightRecord {
        account_discriminator: VoterWeightRecord::ACCOUNT_DISCRIMINATOR,
        realm: *realm_addr,
        governing_token_mint: *realm_token,
        governing_token_owner: *user,
        voter_weight: balance,
        voter_weight_expiry: None,
        weight_action: None,
        weight_action_target: None,
        reserved: [0; 8],
    };

    let (record_addr, record_seeds) = weight_record!(program_id, state_addr, user);
    if *record_acc.key != record_addr {
        msg!("invalid account for weight record");
        return Err(ProgramError::InvalidSeeds);
    }

    const SIZE: u64 = 164;

    create_or_save_account(program_id, record, SIZE, record_acc, funder, &record_seeds)
}

fn update_max_voter_weight<'a, 'b>(
    program_id: &Pubkey,
    state_addr: &Pubkey,
    realm_addr: &Pubkey,
    realm_token: &Pubkey,
    max_weight: u64,
    record_acc: &'a AccountInfo<'b>,
    funder: &'a AccountInfo<'b>,
) -> ProgramResult {
    let record = MaxVoterWeightRecord {
        account_discriminator: MaxVoterWeightRecord::ACCOUNT_DISCRIMINATOR,
        realm: *realm_addr,
        governing_token_mint: *realm_token,
        max_voter_weight: max_weight,
        max_voter_weight_expiry: None,
        reserved: [0; 8],
    };

    let (record_addr, record_seeds) = max_weight_record!(program_id, state_addr);
    if *record_acc.key != record_addr {
        msg!("invalid account for max weight record");
        return Err(ProgramError::InvalidSeeds);
    }

    const SIZE: u64 = 97;

    create_or_save_account(program_id, record, SIZE, record_acc, funder, &record_seeds)
}

fn create_or_save_account<'a, 'b, T: BorshSerialize>(
    program_id: &Pubkey,
    data: T,
    data_size: u64,
    record_acc: &'a AccountInfo<'b>,
    funder: &'a AccountInfo<'b>,
    seeds: &[&[u8]],
) -> ProgramResult {
    if record_acc.owner != program_id {
        let balance = Rent::get()?.minimum_balance(data_size as usize);
        let create_inst =
            create_account(funder.key, record_acc.key, balance, data_size, program_id);

        invoke_signed(
            &create_inst,
            &[funder.clone(), record_acc.clone()],
            &[seeds],
        )?;
    }

    let serialized = data.try_to_vec()?;
    let mut data = record_acc.try_borrow_mut_data()?;

    sol_memcpy(&mut data, &serialized, serialized.len());

    Ok(())
}

// [write] state
// [] token vault
fn process_migrate(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    realm_addr: &Pubkey,
    vault_addr: &Pubkey,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let (mut state, _state_acc) = next_entity::<_, State>(account_info_iter, program_id)?;

    let wallet = next_expected_token_wallet(account_info_iter, &state.wallet)?;

    if state.token_mint != Pubkey::default() {
        msg!("already migrated");
        return Ok(());
    }

    state.realm_addr = *realm_addr;
    state.vault_addr = *vault_addr;
    state.token_mint = wallet.mint;

    Ok(())
}

#[macro_export]
macro_rules! find_keyed_address {
    ($program_id:expr, $($seed:expr),+) => {{
        let (addr, bump) = Pubkey::find_program_address(&[$($seed),+], $program_id);
        (addr, [$($seed),+, &[bump]])
    }};
}

pub const WALLET_SEED: &[u8] = b"WALLET";
pub const VOUCHER_SEED: &[u8] = b"VOUCHER";
pub const VOTE_WEIGHT_SEED: &[u8] = b"VOTE_WEIGHT";
pub const MAX_VOTE_WEIGHT_SEED: &[u8] = b"MAX_VOTE_WEIGHT";

#[macro_export]
macro_rules! voucher {
    ($program_id:expr, $state_addr:expr, $user_addr:expr) => {
        $crate::find_keyed_address!(
            $program_id,
            $crate::VOUCHER_SEED,
            $state_addr.as_ref(),
            $user_addr.as_ref()
        )
    };
}

#[macro_export]
macro_rules! wallet {
    ($program_id:expr, $state_addr:expr) => {
        $crate::find_keyed_address!($program_id, $crate::WALLET_SEED, $state_addr.as_ref())
    };
}

#[macro_export]
macro_rules! weight_record {
    ($program_id:expr, $state_addr:expr, $user_addr:expr) => {
        $crate::find_keyed_address!(
            $program_id,
            $crate::VOTE_WEIGHT_SEED,
            $state_addr.as_ref(),
            $user_addr.as_ref()
        )
    };
}

#[macro_export]
macro_rules! max_weight_record {
    ($program_id:expr, $state_addr:expr) => {
        $crate::find_keyed_address!(
            $program_id,
            $crate::MAX_VOTE_WEIGHT_SEED,
            $state_addr.as_ref()
        )
    };
}

#[cfg(test)]
mod tests {
    use solana_program::sysvar::instructions::{construct_instructions_data, BorrowedInstruction};
    use solana_program::{pubkey::Pubkey, sysvar};

    use super::*;

    #[test]
    fn test_check_no_other_programs() {
        let program_id = Pubkey::new_unique();
        let malicious_program = Pubkey::new_unique();

        call_no_other_program(vec![program_id, program_id], &program_id).unwrap();
        call_no_other_program(vec![program_id, spl_token::ID], &program_id).unwrap_err();
        call_no_other_program(vec![malicious_program, program_id, program_id], &program_id)
            .unwrap_err();

        // test it fails on invalid sysvar ID
        let mut lamports = 0;
        let mut data = [0u8; 4];
        let sysvar = AccountInfo::new(
            &malicious_program,
            false,
            false,
            &mut lamports,
            &mut data,
            &system_program::ID,
            false,
            0,
        );

        check_no_other_programs(&sysvar, &[&program_id]).unwrap_err();
    }

    fn call_no_other_program(programs: Vec<Pubkey>, program_id: &Pubkey) -> ProgramResult {
        let instructions = programs
            .iter()
            .map(|k| BorrowedInstruction {
                program_id: k,
                accounts: Vec::new(),
                data: &[],
            })
            .collect::<Vec<BorrowedInstruction>>();

        let mut message = construct_instructions_data(&instructions);
        let mut lamports = 0;
        let sysvar = AccountInfo::new(
            &sysvar::instructions::ID,
            false,
            false,
            &mut lamports,
            &mut message,
            &system_program::ID,
            false,
            0,
        );

        check_no_other_programs(&sysvar, &[&program_id])
    }
}
