use solana_program::program_error::ProgramError;
use thiserror::Error;

#[derive(Error, Debug)]
#[repr(u32)]
pub enum Error {
    #[error("overflow occured")]
    Overflow,
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
