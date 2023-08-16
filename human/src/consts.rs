use solana_program::clock::UnixTimestamp;

pub const V1: &[u8] = b"HMN_V1";

pub const AUTHORITY_SEED: &[u8] = b"TRANSFER";
pub const STATE_SEED: &[u8] = b"STATE";
pub const WALLET_SEED: &[u8] = b"WALLET"; // drop wallet
pub const VAULT_SEED: &[u8] = b"VAULT"; // vesting wallet (holds 99% supply)
pub const STASH_SEED: &[u8] = b"STASH"; // temp wallet for depositing SOL

// address of master post mint
pub const MASTER_POST_MINT_SEED: &[u8] = b"MASTER_POST";
pub const POST_INFO_SEED: &[u8] = b"POST_INFO";
pub const REPOST_RECORD_SEED: &[u8] = b"REPOST_RECORD";
pub const COLLECTION_MINT_SEED: &[u8] = b"COLLECTION";

pub const BUY_COMMISSION: u16 = 1000; // 10%
pub const TREASURY_COMMISSION: u16 = 8000; // 80%

pub const MAX_REPOST_TIME: i64 = 24 * 60 * 60; // 24h
pub const REPOST_REDEEM_COOLDOWN: i64 = 24 * 60 * 60; // 24h

#[cfg(feature = "dev")]
pub const VESTING_TOTAL_PARTS: u8 = 30;
#[cfg(feature = "dev")]
pub const VESTING_PART_LENGTH: UnixTimestamp = 86400; // seconds in a day

#[cfg(not(feature = "dev"))]
pub const VESTING_TOTAL_PARTS: u8 = 12 * 3; // 12 months across 3 years
#[cfg(not(feature = "dev"))]
pub const VESTING_PART_LENGTH: UnixTimestamp = 2629800; // seconds in a month

pub const DEFAULT_REPORT_PRICE_LAMPORTS: u64 = 10000000; // 0.01 SOL
pub const POST_ROYALTY_COMMISSION_BSP: u16 = 1000; // 10%

pub const FREE_REPOST_RECEIVE_AMOUNT: u64 = 100;
