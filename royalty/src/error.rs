use solana_program::program_error::ProgramError;
use thiserror::Error;

#[derive(Error, Debug)]
#[repr(u32)]
pub enum Error {
    #[error("operation would result in deposited balance less than required minimum")]
    LessThanDepositMinimum,
    #[error("this account is prohibited from enrolling in distribution")]
    BlackistedForEnroll,
    #[error("no other program calls can be present in enumerate transaction")]
    NoOtherProgramsAllowed,
    #[error("voucher is not eligible to receive this drop")]
    VoucherNotEligible,
    #[error("overflow occured")]
    Overflow,
    #[error("inconsistent voucher state (very bad)")]
    InvalidVoucher,
    #[error("not enough lamports on account to start distribution")]
    NotEnoughToStartDistribution,
    #[error("can't start distribution with 0 token balance")]
    ZeroTokenBalance,
    #[error("instruction unavailable while distribution is in progress")]
    TemporaryUnavailable,
    #[error("not enough balance to withdraw")]
    InsufficientBalance,
    #[error("can't withdraw while user's token owner record has unrelinquished votes")]
    UnrelinquishedVotes,
}

impl From<Error> for ProgramError {
    fn from(e: Error) -> Self {
        ProgramError::Custom(e as u32)
    }
}

impl<T> From<Error> for Result<T, ProgramError> {
    fn from(e: Error) -> Self {
        Err(ProgramError::Custom(e as u32))
    }
}
