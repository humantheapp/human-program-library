use solana_program::{
    account_info::{next_account_info, AccountInfo},
    msg,
    program_error::ProgramError,
    program_pack::Pack,
    pubkey::Pubkey,
};

use spl_associated_token_account::get_associated_token_address;
use spl_token::state as token_state;

pub fn next_expected_token_wallet<'a, 'b: 'a, I>(
    i: &mut I,
    wallet_addr: &Pubkey,
) -> Result<token_state::Account, ProgramError>
where
    I: Iterator<Item = &'a AccountInfo<'b>>,
{
    let wallet = next_account_info(i)?;

    if wallet.key != wallet_addr {
        msg!(
            "invalid wallet: expected {} but got {}",
            wallet_addr,
            wallet.key
        );
        return Err(ProgramError::InvalidArgument);
    }

    if !spl_token::check_id(wallet.owner) {
        return Err(ProgramError::IllegalOwner);
    }

    let account = token_state::Account::unpack(&wallet.data.borrow())?;

    Ok(account)
}

pub fn next_atoken_wallet<'a, 'b: 'a, I>(
    i: &mut I,
    expected_user: &Pubkey,
    exprected_mint: &Pubkey,
) -> Result<(Pubkey, token_state::Account), ProgramError>
where
    I: Iterator<Item = &'a AccountInfo<'b>>,
{
    let wallet_acc = next_account_info(i)?;

    let expected = get_associated_token_address(expected_user, exprected_mint);
    if expected != *wallet_acc.key {
        msg!(
            "invalid atoken wallet: expected {} but got {}",
            expected,
            wallet_acc.key
        );
        return Err(ProgramError::InvalidArgument);
    }

    if !spl_token::check_id(wallet_acc.owner) {
        return Err(ProgramError::IllegalOwner);
    }

    let wallet = token_state::Account::unpack(&wallet_acc.try_borrow_data()?)?;

    Ok((*wallet_acc.key, wallet))
}

/// returns next expected account that is signer
pub fn next_signer_account<'a, 'b, I: Iterator<Item = &'a AccountInfo<'b>>>(
    i: &mut I,
    expected_acc: &Pubkey,
) -> Result<&'a AccountInfo<'b>, ProgramError> {
    let account = next_account_info(i)?;

    if account.key != expected_acc {
        return Err(ProgramError::InvalidArgument);
    }

    if !account.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    Ok(account)
}

/// returns next expected account while checking it's address
pub fn next_expected_account<'a, 'b: 'a, I>(
    i: &mut I,
    expected_key: &Pubkey,
) -> Result<I::Item, ProgramError>
where
    I: Iterator<Item = &'a AccountInfo<'b>>,
{
    let acc = next_account_info(i)?;

    if acc.key != expected_key {
        msg!(
            "invalid account: expected {} but got {}",
            expected_key,
            acc.key
        );
        return Err(ProgramError::InvalidArgument);
    }

    Ok(acc)
}
