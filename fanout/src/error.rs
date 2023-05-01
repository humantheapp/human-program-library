use anchor_lang::error_code;

#[error_code]
pub enum FanoutError {
    #[msg("Overflow occured")]
    Overflow,
    #[msg("Invalid member count, should be between 1 and 10")]
    InvalidMemberCount,
    #[msg("Member shares should add up to 10000")]
    InvalidShares,
    #[msg("Member seen more than once")]
    DuplicateMember,
    #[msg("Member is not present in distribution")]
    MemberNotFound,
    #[msg("Cannot close fanout with non-zero balance")]
    NonZeroBalance,
    #[msg("Close authority is not the signer")]
    InvalidCloseAuthority,
}
