#![forbid(unsafe_code)]
#![deny(clippy::all)]
#![deny(clippy::integer_arithmetic)]
#![cfg(not(feature = "no-entrypoint"))]

mod error;

use std::mem::{self};

use borsh::{BorshDeserialize, BorshSerialize};
use error::Error;
use human_common::utils::{
    next_atoken_wallet, next_expected_account, next_expected_token_wallet, next_signer_account,
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::{Clock, UnixTimestamp},
    entrypoint,
    entrypoint::ProgramResult,
    msg,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    program_memory::{sol_memcpy, sol_memset},
    program_option::COption,
    program_pack::Pack,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction, system_program,
    sysvar::Sysvar,
};
use spl_associated_token_account::get_associated_token_address;
use spl_token::instruction as token_inst;
use spl_token::state as token_state;

#[derive(Debug, BorshDeserialize, BorshSerialize)]
enum Request {
    /// Request with fixed amount
    Funded(FundedRequest),
    /// Request with variable amount
    Unfunded(UnfundedRequest),
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
struct FundedRequest {
    // system account
    author: Pubkey,
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
struct UnfundedRequest {
    /// amount of token accumulated. needed in case of refund
    collected: u64,
    deadline: Option<UnixTimestamp>,
    accept_threshold: u64,
}

#[repr(u8)]
#[derive(Debug, BorshDeserialize, BorshSerialize, PartialEq)]
enum RequestStatus {
    Unitialized,
    Open,
    Declined,
    Accepted,
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
struct State {
    request_status: RequestStatus,
    /// the moment request was created, so later we could add logic to close stale requests
    created_at: UnixTimestamp,
    /// temporary token wallet
    wallet: Pubkey,
    /// system account. if accepted, where tokens would go
    destination: Pubkey,
    /// account to refund SOL from closing state
    payer: Pubkey,
    /// request body
    request: Request,
}

#[derive(Debug, BorshDeserialize, BorshSerialize, PartialEq)]
struct Voucher {
    /// state address will be used for reverse RPC lookup (getProgramAccounts)
    state: Pubkey,
    /// user (again, for reverse lookup)
    user: Pubkey,
    /// amount contributed by user
    amount: u64,
}

impl State {
    /// returns whether unfunded request if expired. funded requests cannot expire
    fn expired(&self, now: UnixTimestamp) -> bool {
        if !self.is_open() {
            return false;
        }

        if let Request::Unfunded(UnfundedRequest {
            deadline: Some(deadline),
            collected,
            accept_threshold,
        }) = self.request
        {
            return now > deadline && collected < accept_threshold;
        }

        false
    }

    fn try_accept(&mut self) -> Result<(), ProgramError> {
        if self.request_status != RequestStatus::Open {
            return Err(ProgramError::Custom(0x16));
        }

        if let Request::Unfunded(request) = &self.request {
            if request.collected < request.accept_threshold {
                msg!("not enough collected to accept");
                return Err(ProgramError::Custom(0x16));
            }
        }

        self.request_status = RequestStatus::Accepted;

        Ok(())
    }

    fn is_funded(&self) -> bool {
        matches!(self.request, Request::Funded(_))
    }

    fn is_open(&self) -> bool {
        self.request_status == RequestStatus::Open
    }
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
#[repr(u8)]
#[non_exhaustive]
enum Instruction {
    Create(CreateInstruction),

    // only for unfunded
    // transfers all delegated balance as amount
    // emits created token to atoken wallet where close_authority == our derived authority
    // **discuss**: we can't automatically refund you (and receive our SOL's back) unless we are the owner of account
    //      possible options:
    //          require handing ownership (which kinda defeats all purpose of token)
    //          creating another scheme for atoken wallets or new kind of account entirely
    //          wait for all users to sign refund option (not happening)
    Contribute(ContributeInstruction),

    // refund contribution for contribution token. burn token and close account
    // can also be used to close accounts of accepted requests
    // if all collected == 0: erase state.
    Refund,

    // only for funded requests. made by author
    // all funds go back to author. state erased
    Cancel,

    // for creator: receive all wallet funds.
    // if funded: erase state
    Accept,

    // for creator: refund all tokens
    // if funded: erase state
    Decline,
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
struct ContributeInstruction {
    user: Pubkey,
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
struct CreateInstruction {
    dest: Pubkey,
    payer: Pubkey,
    rtype: CreateInstructionRequest,
}

#[derive(Debug, BorshDeserialize, BorshSerialize)]
enum CreateInstructionRequest {
    Funded {
        author: Pubkey,
    },
    Unfunded {
        deadline: Option<UnixTimestamp>,
        accept_threshold: u64,
    },
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
        Instruction::Create(inst) => {
            msg!("creating funded request");
            process_create(program_id, accounts, inst)
        }
        Instruction::Contribute(ContributeInstruction { user }) => {
            msg!("contributing to unfunded request");
            process_contribute(program_id, accounts, &user)
        }
        Instruction::Refund => {
            msg!("refunding request");
            process_refund(program_id, accounts)
        }

        Instruction::Accept => {
            msg!("accepting request");
            process_accept(program_id, accounts)
        }
        Instruction::Decline => {
            msg!("rejecting redeem request");
            process_decline(program_id, accounts)
        }
        Instruction::Cancel => {
            msg!("cancelling funded redeem request");
            process_cancel(program_id, accounts)
        }
    }
}

pub const V1: &[u8] = b"HMN_R1";
pub const AUTHORITY_SEED: &[u8] = b"A";

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

// mix state_addr into the mix, so same token can't be reused in different requests (generally a bad thing)
#[macro_export]
macro_rules! authority {
    ($program_id:expr, $state_addr:expr) => {
        $crate::find_keyed_address!($program_id, AUTHORITY_SEED, $state_addr.as_ref())
    };
}

const STATE_SIZE: usize = 138;

// [writable] new state account owned by this program
// [writable] new wallet with owner and close authority set to derived authority with tokens already on it
fn process_create(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    args: CreateInstruction,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();

    let state_acc = next_account_info(account_info_iter)?;
    let (wallet_addr, wallet) =
        next_owned_token_wallet(account_info_iter, program_id, state_acc.key)?;

    let rent = Rent::get()?;
    let clock = Clock::get()?;

    let mut state_data = state_acc.try_borrow_mut_data()?;

    if !rent.is_exempt(state_acc.lamports(), STATE_SIZE) {
        return Err(ProgramError::AccountNotRentExempt);
    }

    // sanity check
    match args.rtype {
        CreateInstructionRequest::Funded { .. } if wallet.amount == 0 => {
            // should contain some funds (can't create empty request)
            return Err(ProgramError::InsufficientFunds);
        }
        CreateInstructionRequest::Unfunded { .. } if wallet.amount != 0 => {
            // should NOT contain any funds because we are unable to track them
            return Err(ProgramError::InvalidAccountData);
        }
        _ => {}
    }

    let request = match args.rtype {
        CreateInstructionRequest::Funded { author } => Request::Funded(FundedRequest { author }),
        CreateInstructionRequest::Unfunded {
            deadline,
            accept_threshold,
        } => Request::Unfunded(UnfundedRequest {
            collected: 0,
            deadline,
            accept_threshold,
        }),
    };

    let state = State {
        request_status: RequestStatus::Open,
        created_at: clock.unix_timestamp,
        wallet: wallet_addr,
        payer: args.payer,
        destination: args.dest,
        request,
    };

    write_state(&mut state_data, state)?;

    Ok(())
}

fn write_state(data: &mut [u8], state: State) -> Result<(), ProgramError> {
    if data.len() < STATE_SIZE {
        return Err(ProgramError::AccountDataTooSmall);
    }

    // is this enough?
    if matches!(State::try_from_slice(data), Ok(State {  request_status, .. }) if request_status != RequestStatus::Unitialized)
    {
        return Err(ProgramError::AccountAlreadyInitialized);
    }

    let state = state.try_to_vec()?;

    copy_slice(data, &state);

    Ok(())
}

pub fn next_owned_token_wallet<'a, 'b: 'a, I>(
    i: &mut I,
    program_id: &Pubkey,
    state_addr: &Pubkey,
) -> Result<(Pubkey, token_state::Account), ProgramError>
where
    I: Iterator<Item = &'a AccountInfo<'b>>,
{
    let wallet = next_account_info(i)?;

    if !spl_token::check_id(wallet.owner) {
        return Err(ProgramError::InvalidArgument);
    }

    let account = token_state::Account::unpack(&wallet.data.borrow())?;

    let (derived_authority, _) = authority!(program_id, state_addr);
    if account.owner != derived_authority {
        return Err(ProgramError::IllegalOwner);
    }

    if account.close_authority.is_some() {
        return Err(ProgramError::IllegalOwner);
    }

    Ok((*wallet.key, account))
}

fn next_state_account<'a, 'b, I: Iterator<Item = &'a AccountInfo<'b>>>(
    i: &mut I,
    program_id: &Pubkey,
) -> Result<(State, &'a AccountInfo<'b>), ProgramError> {
    let state_acc = next_account_info(i)?;

    if state_acc.owner != program_id {
        msg!(
            "illegal state owner ({} != {})",
            state_acc.owner,
            program_id
        );
        return Err(ProgramError::IllegalOwner);
    }

    let data = state_acc.try_borrow_data()?;

    let state = State::try_from_slice(&data)?;

    if state.request_status == RequestStatus::Unitialized {
        return Err(ProgramError::UninitializedAccount);
    }

    Ok((state, state_acc))
}

fn save_state(state: State, state_acc: &AccountInfo) -> Result<(), ProgramError> {
    let serialized = state.try_to_vec()?;

    let mut data = state_acc.try_borrow_mut_data()?;

    copy_slice(&mut data[..], &serialized);

    Ok(())
}

fn erase_state(
    state: State,
    state_acc: &AccountInfo,
    payer_acc: &AccountInfo,
) -> Result<(), ProgramError> {
    // sanity check
    assert_ne!(state.request_status, RequestStatus::Open);

    if let Request::Unfunded(UnfundedRequest { collected, .. }) = state.request {
        assert_eq!(collected, 0, "error: not all vouchers are refunded")
    };

    // withdraw all lamports from state
    let lamports = state_acc.lamports();

    **state_acc.try_borrow_mut_lamports()? = 0;
    let mut payer_lamports = payer_acc.try_borrow_mut_lamports()?;
    **payer_lamports = payer_lamports
        .checked_add(lamports)
        .ok_or(Error::Overflow)?;

    let mut data = state_acc.try_borrow_mut_data()?;
    let l = data.len();
    sol_memset(&mut data, 0, l);

    Ok(())
}

#[inline]
fn copy_slice(dst: &mut [u8], src: &[u8]) {
    sol_memcpy(dst, src, src.len());
}

// [writable] request state
// [writable] request wallet
// [writable] token wallet with delegated amount
// [sign] delegate
// [writable] voucher account
// [] rent var
// [] clock var
fn process_contribute(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    user: &Pubkey,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();

    let (mut state, state_acc) = next_state_account(account_info_iter, program_id)?;
    let _wallet = next_expected_token_wallet(account_info_iter, &state.wallet)?;

    let source_wallet = next_signer_account(account_info_iter, &state.destination)?;

    if !spl_token::check_id(source_wallet.owner) {
        return Err(ProgramError::IllegalOwner);
    }

    let source_wallet_data = token_state::Account::unpack(&source_wallet.try_borrow_data()?)?;

    let amount = source_wallet_data.delegated_amount;
    if amount == 0 {
        return Err(ProgramError::InvalidArgument);
    }

    let delegate = source_wallet_data
        .delegate
        .ok_or(ProgramError::InvalidArgument)?;

    next_signer_account(account_info_iter, &delegate)?;

    let voucher_acc = next_account_info(account_info_iter)?;
    let rent = Rent::from_account_info(next_account_info(account_info_iter)?)?;
    let clock = Clock::from_account_info(next_account_info(account_info_iter)?)?;

    if state.expired(clock.unix_timestamp) {
        state.request_status = RequestStatus::Declined;
        save_state(state, state_acc)?;
        return Err(ProgramError::Custom(0x17));
    }

    if !state.is_open() {
        return Err(ProgramError::Custom(0x12));
    }

    let mut unfunded_state = match state.request {
        Request::Funded(_) => return Err(ProgramError::Custom(0x12)),
        Request::Unfunded(ref mut s) => s,
    };

    // transfer user amount to
    let inst = token_inst::transfer(
        &spl_token::ID,
        source_wallet.key,
        &state.wallet,
        &delegate,
        &[],
        amount,
    )?;

    invoke(&inst, accounts)?;

    // issue voucher (or update amount on existing one)
    let mut previous_amount = 0;

    if let Some(v) = get_voucher(voucher_acc, program_id, &rent)? {
        if v.user != *user {
            msg!("refusing to override another user's voucher");
            return Err(ProgramError::IllegalOwner);
        }

        previous_amount = v.amount
    }

    let voucher = Voucher {
        state: *state_acc.key,
        user: *user,
        amount: previous_amount.checked_add(amount).ok_or(Error::Overflow)?,
    }
    .try_to_vec()?;

    let mut data = voucher_acc.try_borrow_mut_data()?;
    copy_slice(&mut data, &voucher);

    // update total contributed counter
    unfunded_state.collected = unfunded_state
        .collected
        .checked_add(amount)
        .ok_or(Error::Overflow)?;

    save_state(state, state_acc)?;

    Ok(())
}

fn get_voucher(
    voucher_acc: &AccountInfo,
    program_id: &Pubkey,
    rent: &Rent,
) -> Result<Option<Voucher>, ProgramError> {
    if !voucher_acc.is_writable {
        return Err(ProgramError::InvalidArgument);
    }

    if voucher_acc.owner != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }

    if !rent.is_exempt(voucher_acc.lamports(), voucher_acc.data_len()) {
        return Err(ProgramError::AccountNotRentExempt);
    }

    let data = voucher_acc.try_borrow_data()?;

    match Voucher::try_from_slice(&data) {
        Ok(v) if v.amount > 0 => Ok(Some(v)),
        _ => Ok(None),
    }
}

// [writable] request state
// [writable] request wallet
// [writable] payer
// [] rent var
// [] clock var
// for n..10:
// [writable] voucher
// [writable] user atoken wallet
fn process_refund(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter().peekable();

    let (mut state, state_acc) = next_state_account(account_info_iter, program_id)?;
    let wallet = next_expected_token_wallet(account_info_iter, &state.wallet)?;

    let payer = next_account_info(account_info_iter)?;
    let rent = Rent::from_account_info(next_account_info(account_info_iter)?)?;
    let _clock = Clock::from_account_info(next_account_info(account_info_iter)?)?;

    if *payer.key != state.payer {
        // not fair
        return Err(ProgramError::IllegalOwner);
    }

    if state.is_open() {
        return Err(ProgramError::InvalidArgument);
    }

    let request = match state.request {
        Request::Funded(_) => return Err(ProgramError::Custom(0x12)),
        Request::Unfunded(ref mut s) => s,
    };

    let (derived_authority, authority_seed) = authority!(program_id, state_acc.key);

    let mut payer_lamports = payer.try_borrow_mut_lamports()?;

    // get our sweet money back
    while account_info_iter.peek().is_some() {
        let voucher_acc = next_account_info(account_info_iter)?;

        let voucher = get_voucher(voucher_acc, program_id, &rent)?
            .ok_or(ProgramError::UninitializedAccount)?;

        // close voucher
        redeem_voucher(voucher_acc, &mut payer_lamports)?;

        request.collected = request
            .collected
            .checked_sub(voucher.amount)
            .ok_or(ProgramError::Custom(0x14))?;

        let user_wallet = next_account_info(account_info_iter)?;

        let derived_wallet = get_associated_token_address(&voucher.user, &wallet.mint);
        if derived_wallet != *user_wallet.key {
            return Err(ProgramError::InvalidArgument);
        }

        if state.request_status == RequestStatus::Accepted {
            // nothing to refund
            continue;
        }

        // refund user
        let transfer = token_inst::transfer(
            &spl_token::ID,
            &state.wallet,
            user_wallet.key,
            &derived_authority,
            &[],
            voucher.amount,
        )?;

        invoke_signed(&transfer, accounts, &[authority_seed])?;
    }

    if request.collected > 0 {
        // some refunding still required
        return Ok(());
    }

    if wallet.amount != 0 {
        msg!("sanity check failed: collected == 0 but token amount is still not zero");
        return Err(ProgramError::Custom(0x15));
    }

    // all accounts closed and refunded
    let close = token_inst::close_account(
        &spl_token::ID,
        &state.wallet,
        payer.key,
        &derived_authority,
        &[],
    )?;

    invoke_signed(&close, accounts, &[authority_seed])?;

    erase_state(state, state_acc, payer)?;

    Ok(())
}

fn redeem_voucher(state_acc: &AccountInfo, payer_lamports: &mut u64) -> Result<(), ProgramError> {
    // withdraw all lamports from state
    *payer_lamports = payer_lamports
        .checked_add(mem::take(*state_acc.try_borrow_mut_lamports()?))
        .ok_or(Error::Overflow)?;

    let mut data = state_acc.try_borrow_mut_data()?;
    let l = data.len();
    sol_memset(&mut data, 0, l);

    Ok(())
}

// [writable] request state
// [writable] request wallet
// [sign] destination account
// [writable] destination token wallet
// [writable] payer
// [] clock var
// [] derived authority
// [] token program
fn process_accept(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();

    let (mut state, state_acc) = next_state_account(account_info_iter, program_id)?;
    let wallet = next_expected_token_wallet(account_info_iter, &state.wallet)?;

    let _dest = next_signer_account(account_info_iter, &state.destination)?;
    let destination_wallet = next_account_info(account_info_iter)?;

    let payer = next_account_info(account_info_iter)?;
    let clock = Clock::from_account_info(next_account_info(account_info_iter)?)?;

    if *payer.key != state.payer {
        // not fair
        return Err(ProgramError::IllegalOwner);
    }

    if state.expired(clock.unix_timestamp) {
        state.request_status = RequestStatus::Declined;
        save_state(state, state_acc)?;
        return Err(ProgramError::Custom(0x17));
    }

    if !state.is_open() {
        return Err(ProgramError::Custom(0x10));
    }

    // sanity check
    if wallet.amount == 0 {
        return Err(ProgramError::Custom(0x11));
    }

    state.try_accept()?;

    let (derived_authority, authority_seed) = authority!(program_id, state_acc.key);

    // transfer tokens to dest
    let transfer = token_inst::transfer(
        &spl_token::ID,
        &state.wallet,
        destination_wallet.key,
        &derived_authority,
        &[],
        wallet.amount,
    )?;

    invoke_signed(&transfer, accounts, &[authority_seed])?;

    if state.is_funded() {
        // close token wallet
        let close = token_inst::close_account(
            &spl_token::ID,
            &state.wallet,
            payer.key,
            &derived_authority,
            &[],
        )?;

        invoke_signed(&close, accounts, &[authority_seed])?;

        // we're done here. return our lamports
        erase_state(state, state_acc, payer)?;
        return Ok(());
    }

    // unfunded requests stay until every single contributor closed his account
    save_state(state, state_acc)?;

    Ok(())
}

// [writable] request state
// [writable] request wallet
// [sign] author
// [writable] atoken address of author
// [writable] payer
// [] derived authority
fn process_cancel(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();

    let (mut state, state_acc) = next_state_account(account_info_iter, program_id)?;
    let wallet = next_expected_token_wallet(account_info_iter, &state.wallet)?;

    let author = match state.request {
        Request::Funded(FundedRequest { author }) => author,
        Request::Unfunded { .. } => return Err(ProgramError::Custom(0x13)),
    };

    let _dest = next_signer_account(account_info_iter, &author)?;
    let payer = next_expected_account(account_info_iter, &state.payer)?;

    if !state.is_open() {
        return Err(ProgramError::Custom(0x10));
    }

    state.request_status = RequestStatus::Declined;
    let (derived_authority, authority_seed) = authority!(program_id, state_acc.key);
    let _authority = next_expected_account(account_info_iter, &derived_authority)?;

    if let COption::Some(rent_balance) = wallet.is_native {
        let atoken_addr = get_associated_token_address(&author, &wallet.mint);
        next_expected_account(account_info_iter, &atoken_addr)?;
        next_expected_account(account_info_iter, &spl_token::ID)?;
        next_expected_account(account_info_iter, &system_program::ID)?;

        // close token wallet
        let close = token_inst::close_account(
            &spl_token::ID,
            &state.wallet,
            &derived_authority,
            &derived_authority,
            &[],
        )?;
        invoke_signed(&close, accounts, &[authority_seed])?;

        // system transfer amount to author
        msg!("transfer sol back to dest");
        let transfer = system_instruction::transfer(&derived_authority, &author, wallet.amount);
        invoke_signed(&transfer, accounts, &[authority_seed])?;

        // transfer rent exemption to payer
        msg!("transfer rent back to payer");
        let transfer = system_instruction::transfer(&derived_authority, payer.key, rent_balance);
        invoke_signed(&transfer, accounts, &[authority_seed])?;
    } else {
        let (author_wallet, _) = next_atoken_wallet(account_info_iter, &author, &wallet.mint)?;
        next_expected_account(account_info_iter, &spl_token::ID)?;

        // transfer tokens to dest
        msg!("transfer tokens back to dest");
        let transfer = token_inst::transfer(
            &spl_token::ID,
            &state.wallet,
            &author_wallet,
            &derived_authority,
            &[],
            wallet.amount,
        )?;

        invoke_signed(&transfer, accounts, &[authority_seed])?;

        msg!("close token wallet");
        // close token wallet
        let close = token_inst::close_account(
            &spl_token::ID,
            &state.wallet,
            payer.key,
            &derived_authority,
            &[],
        )?;
        invoke_signed(&close, accounts, &[authority_seed])?;
    }
    erase_state(state, state_acc, payer)?;

    Ok(())
}

// [writable] request
// [writable] request wallet
// [sign] destination account
// [writable] payer
// [optional, writable] if request is funded, atoken adress of author
// [writable] if request is funded, author address
// [] authority
// [] token prog
fn process_decline(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();

    let (mut state, state_acc) = next_state_account(account_info_iter, program_id)?;
    let wallet = next_expected_token_wallet(account_info_iter, &state.wallet)?;

    let _dest = next_signer_account(account_info_iter, &state.destination)?;
    let payer = next_expected_account(account_info_iter, &state.payer)?;

    if !state.is_open() {
        return Err(ProgramError::Custom(0x10));
    }

    state.request_status = RequestStatus::Declined;

    match state.request {
        Request::Funded(ref r) => {
            let (derived_authority, authority_seed) = authority!(program_id, state_acc.key);
            //let _authority = next_expected_account(account_info_iter, &derived_authority)?;

            if let COption::Some(rent_balance) = wallet.is_native {
                let _author = next_expected_account(account_info_iter, &r.author)?;

                // close token wallet
                let close = token_inst::close_account(
                    &spl_token::ID,
                    &state.wallet,
                    &derived_authority,
                    &derived_authority,
                    &[],
                )?;
                invoke_signed(&close, accounts, &[authority_seed])?;

                next_expected_account(account_info_iter, &spl_token::ID)?; // 8
                next_expected_account(account_info_iter, &system_program::ID)?; // 9

                // system transfer amount to author
                let transfer =
                    system_instruction::transfer(&derived_authority, &r.author, wallet.amount);
                invoke_signed(&transfer, accounts, &[authority_seed])?;

                // transfer rent exemption to payer
                let transfer =
                    system_instruction::transfer(&derived_authority, payer.key, rent_balance);
                invoke_signed(&transfer, accounts, &[authority_seed])?;
            } else {
                let (author_wallet, _) =
                    next_atoken_wallet(account_info_iter, &r.author, &wallet.mint)?;

                next_expected_account(account_info_iter, &r.author)?;

                // transfer tokens to dest
                let transfer = token_inst::transfer(
                    &spl_token::ID,
                    &state.wallet,
                    &author_wallet,
                    &derived_authority,
                    &[],
                    wallet.amount,
                )?;

                invoke_signed(&transfer, accounts, &[authority_seed])?;

                // close token wallet
                let close = token_inst::close_account(
                    &spl_token::ID,
                    &state.wallet,
                    payer.key,
                    &derived_authority,
                    &[],
                )?;

                invoke_signed(&close, accounts, &[authority_seed])?;
            }

            // we're done here. return our lamports
            erase_state(state, state_acc, payer)?;
            Ok(())
        }
        Request::Unfunded(_) => {
            // unfunded requests stay until every single contributor has closed an account
            save_state(state, state_acc)?;
            Ok(())
        }
    }
}
#[cfg(test)]
mod tests {
    use solana_program::pubkey::Pubkey;

    use crate::{
        Request::{self, *},
        *,
    };

    #[test]
    fn test_state_is_funded() {
        assert!(
            from_request(Unfunded(UnfundedRequest {
                collected: 0,
                deadline: None,
                accept_threshold: 0,
            }))
            .is_funded()
                == false
        );

        assert!(
            from_request(Funded(FundedRequest {
                author: Pubkey::default(),
            }))
            .is_funded()
                == true
        );
    }

    #[test]
    fn test_state_expired() {
        let now = 100000000;

        let expired = from_request(Unfunded(UnfundedRequest {
            collected: 1000,
            deadline: Some(now - 100), // dealine has passed
            accept_threshold: 100000,
        }));

        assert!(expired.expired(now) == true);

        //
        let funded_in_time = from_request(Unfunded(UnfundedRequest {
            collected: 9999,
            deadline: Some(now - 100), // dealine has passed but enough was collected
            accept_threshold: 1000,
        }));

        assert!(funded_in_time.expired(now) == false);

        let not_expired = from_request(Unfunded(UnfundedRequest {
            collected: 1000,
            deadline: Some(now + 100), // deadline has not yet passed
            accept_threshold: 100000,
        }));

        assert!(not_expired.expired(now) == false);

        let no_deadline = from_request(Unfunded(UnfundedRequest {
            collected: 1000,
            deadline: None, // no deadline
            accept_threshold: 100000,
        }));

        assert!(no_deadline.expired(now) == false);
    }

    #[test]
    fn test_state_try_accept() {
        let mut funded = from_request(Funded(FundedRequest {
            author: Pubkey::default(),
        }));

        // funded can always be accepted
        funded.try_accept().unwrap();
        assert!(!funded.is_open());

        let mut unfunded = from_request(Unfunded(UnfundedRequest {
            collected: 1000,
            deadline: None,
            accept_threshold: 100000,
        }));

        unfunded.try_accept().unwrap_err();

        assert_eq!(unfunded.request_status, RequestStatus::Open);
        assert!(unfunded.is_open());

        let mut acceptable = from_request(Unfunded(UnfundedRequest {
            collected: 77777,
            deadline: None,
            accept_threshold: 10,
        }));

        acceptable.try_accept().unwrap();

        assert_eq!(acceptable.request_status, RequestStatus::Accepted);
        assert!(!acceptable.is_open());
    }

    fn from_request(request: Request) -> State {
        let s = State {
            request_status: RequestStatus::Open,
            wallet: Pubkey::default(),
            destination: Pubkey::default(),
            payer: Pubkey::default(),
            created_at: 0,
            request,
        };

        // assert_eq!(size_of::<State>(), 0);
        // assert_eq!(s.try_to_vec().unwrap().len(), 0);

        s
    }

    #[test]
    fn test_get_voucher() {
        let rent = Rent::with_slots_per_epoch(432000);

        let data_len = 72;
        let mut data = vec![0; data_len];
        let mut lamports = rent.minimum_balance(data_len);

        let key = Pubkey::new_from_array([1; 32]);
        let pid = Pubkey::new_from_array([23; 32]);

        // not initialized
        {
            let acc = AccountInfo::new(&key, false, true, &mut lamports, &mut data, &pid, false, 0);
            assert!(get_voucher(&acc, &pid, &rent).unwrap().is_none());
        }

        {
            // invalid pid
            let acc = AccountInfo::new(&key, false, true, &mut lamports, &mut data, &pid, false, 0);
            assert_eq!(
                get_voucher(&acc, &Pubkey::default(), &rent).unwrap_err(),
                ProgramError::IncorrectProgramId
            );
        }

        {
            // not rent exempt
            let mut ins = 12;

            let acc = AccountInfo::new(&key, false, true, &mut ins, &mut data, &pid, false, 0);
            assert_eq!(
                get_voucher(&acc, &pid, &rent).unwrap_err(),
                ProgramError::AccountNotRentExempt
            );
        }

        {
            // happy path
            let state_addr = Pubkey::new_from_array([66; 32]);
            let user_addr = Pubkey::new_from_array([77; 32]);

            let v = Voucher {
                state: state_addr,
                user: user_addr,
                amount: 123,
            };

            let mut data = v.try_to_vec().unwrap();

            let acc = AccountInfo::new(&key, false, true, &mut lamports, &mut data, &pid, false, 0);
            assert_eq!(get_voucher(&acc, &pid, &rent).unwrap().unwrap(), v);
        }
    }
}
