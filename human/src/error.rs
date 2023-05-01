use solana_program::program_error::ProgramError;
use thiserror::Error;

#[derive(Error, Debug)]
#[repr(u32)]
pub enum Error {
    #[error("overflow occured")]
    Overflow,
    #[error("drop price can't be zero")]
    DropPriceZero,
    #[error("drop start and end dates overlap")]
    DropInvalidDate,
    #[error("drop already in progress")]
    DropAlreadyExists,
    #[error("no drop in progress")]
    NoDrop,
    #[error("drop timeframe has expired")]
    DropTimeframeExpired,
    #[error("actual price is greater than expected")]
    ExpectedPriceMismatch,
    #[error("post repost window expired")]
    RepostWindowExpired,
    #[error("can't redeem yet")]
    CantRedeemNow,
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
