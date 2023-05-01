use std::str::FromStr;

use human_common::entity::Entity;
use solana_program::{program_pack::Pack, pubkey::Pubkey, rent::Rent, system_instruction};

use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::{account::Account, signature::Keypair};

use human_royalty::{
    governance_program,
    instruction::{deposit, initialize, sync_weight_record, withdraw, InitializeArgs},
    max_weight_record, process_instruction,
    state::{Settings, State},
    weight_record,
};
use spl_associated_token_account::get_associated_token_address;
use spl_governance::state::{
    enums::GovernanceAccountType,
    token_owner_record::{self, TokenOwnerRecordV2},
};
use spl_governance_addin_api::{
    max_voter_weight::MaxVoterWeightRecord, voter_weight::VoterWeightRecord,
};

use {
    solana_program_test::*,
    solana_sdk::{signature::Signer, transaction::Transaction},
};

#[tokio::test]
async fn test() {
    test_transaction().await;
}

async fn test_transaction() {
    let program_id = Pubkey::from_str("22222222222222222222222222222222222222222222").unwrap();

    let realm_addr = Pubkey::from_str("222222222222222222222222222222222222222rea1m").unwrap();

    let mut pt = ProgramTest::new(
        "human_royalty_contract",
        program_id,
        processor!(process_instruction),
    );

    let owner_acc = Keypair::new();
    let host_acc = Keypair::new();
    let mint_acc = Keypair::new();
    let user_acc = Keypair::new();
    let vault = Keypair::new();

    let token_owner_record = token_owner_record::get_token_owner_record_address(
        &governance_program::ID,
        &realm_addr,
        &mint_acc.pubkey(),
        &user_acc.pubkey(),
    );

    let data = TokenOwnerRecordV2 {
        account_type: GovernanceAccountType::TokenOwnerRecordV2,
        realm: realm_addr,
        governing_token_mint: mint_acc.pubkey(),
        governing_token_owner: user_acc.pubkey(),
        governing_token_deposit_amount: 0,
        unrelinquished_votes_count: 0, // important
        outstanding_proposal_count: 1,
        version: 1,
        reserved: [0; 6],
        governance_delegate: None,
        reserved_v2: [0; 128],
    }
    .try_to_vec()
    .unwrap();

    pt.add_account(
        token_owner_record,
        Account {
            lamports: 1000,
            data,
            owner: governance_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let (mut banks_client, fee_payer, recent_blockhash) = pt.start().await;

    let rent = Rent::default();

    let mint_size = spl_token::state::Mint::LEN;

    let create_mint = solana_program::system_instruction::create_account(
        &fee_payer.pubkey(),
        &mint_acc.pubkey(),
        rent.minimum_balance(mint_size),
        mint_size as u64,
        &spl_token::ID,
    );

    let initialize_mint = spl_token::instruction::initialize_mint(
        &spl_token::ID,
        &mint_acc.pubkey(),
        &fee_payer.pubkey(),
        None,
        4,
    )
    .unwrap();

    let atoken = get_associated_token_address(&user_acc.pubkey(), &mint_acc.pubkey());
    let owner_atoken = get_associated_token_address(&owner_acc.pubkey(), &mint_acc.pubkey());

    let create_atoken = spl_associated_token_account::create_associated_token_account(
        &fee_payer.pubkey(),
        &user_acc.pubkey(),
        &mint_acc.pubkey(),
    );

    let mint = spl_token::instruction::mint_to(
        &spl_token::ID,
        &mint_acc.pubkey(),
        &atoken,
        &fee_payer.pubkey(),
        &[],
        10_000,
    )
    .unwrap();

    let create_atoken2 = spl_associated_token_account::create_associated_token_account(
        &fee_payer.pubkey(),
        &owner_acc.pubkey(),
        &mint_acc.pubkey(),
    );

    let mint_owner = spl_token::instruction::mint_to(
        &spl_token::ID,
        &mint_acc.pubkey(),
        &owner_atoken,
        &fee_payer.pubkey(),
        &[],
        3_000,
    )
    .unwrap();

    let allocate_vault = system_instruction::create_account(
        &fee_payer.pubkey(),
        &vault.pubkey(),
        rent.minimum_balance(spl_token::state::Account::LEN),
        spl_token::state::Account::LEN as u64,
        &spl_token::ID,
    );

    let create_vault = spl_token::instruction::initialize_account(
        &spl_token::ID,
        &vault.pubkey(),
        &mint_acc.pubkey(),
        &owner_acc.pubkey(),
    )
    .unwrap();

    let mint_vault = spl_token::instruction::mint_to(
        &spl_token::ID,
        &mint_acc.pubkey(),
        &vault.pubkey(),
        &fee_payer.pubkey(),
        &[],
        5_000,
    )
    .unwrap();

    let mut transaction = Transaction::new_with_payer(
        &[
            create_mint,
            initialize_mint,
            create_atoken,
            create_atoken2,
            mint,
            mint_owner,
            allocate_vault,
            create_vault,
            mint_vault,
        ],
        Some(&fee_payer.pubkey()),
    );

    transaction.sign(&[&fee_payer, &mint_acc, &vault], recent_blockhash);

    banks_client
        .process_transaction(transaction)
        .await
        .expect("process create mint");

    // initialize state
    let state_acc = Keypair::new();
    let create_state = system_instruction::create_account(
        &fee_payer.pubkey(),
        &state_acc.pubkey(),
        rent.minimum_balance(State::SIZE),
        State::SIZE as u64,
        &program_id,
    );

    let args = InitializeArgs {
        owner: owner_acc.pubkey(),
        host: host_acc.pubkey(),
        settings: Settings {
            min_token_to_enroll: 5000,
            owner_fee: 1000,
            host_fee: 100,
            host_flat_fee: 5000,
        },
        realm_addr,
        vault_addr: vault.pubkey(),
    };

    let initialize = initialize(
        &program_id,
        &state_acc.pubkey(),
        &mint_acc.pubkey(),
        &fee_payer.pubkey(),
        args,
    );

    let mut transaction =
        Transaction::new_with_payer(&[create_state, initialize], Some(&fee_payer.pubkey()));

    transaction.sign(&[&fee_payer, &state_acc], recent_blockhash);

    banks_client
        .process_transaction(transaction)
        .await
        .expect("process create mint");

    let deposit = deposit(
        &program_id,
        &state_acc.pubkey(),
        &vault.pubkey(),
        &mint_acc.pubkey(),
        &user_acc.pubkey(),
        &owner_acc.pubkey(),
        &fee_payer.pubkey(),
        10_000,
    );

    let sync = sync_weight_record(
        &program_id,
        &state_acc.pubkey(),
        &vault.pubkey(),
        &owner_acc.pubkey(),
        &mint_acc.pubkey(),
        &user_acc.pubkey(),
        &fee_payer.pubkey(),
    );

    let sync_owner = sync_weight_record(
        &program_id,
        &state_acc.pubkey(),
        &vault.pubkey(),
        &owner_acc.pubkey(),
        &mint_acc.pubkey(),
        &owner_acc.pubkey(),
        &fee_payer.pubkey(),
    );

    let mut transaction =
        Transaction::new_with_payer(&[deposit, sync, sync_owner], Some(&fee_payer.pubkey()));

    transaction.sign(&[&fee_payer, &user_acc], recent_blockhash);

    banks_client
        .process_transaction(transaction)
        .await
        .expect("process initialize");

    assert_weight_record(
        &program_id,
        &state_acc,
        &realm_addr,
        &mint_acc,
        &user_acc,
        &mut banks_client,
        10000,
    )
    .await;

    assert_weight_record(
        &program_id,
        &state_acc,
        &realm_addr,
        &mint_acc,
        &owner_acc,
        &mut banks_client,
        8000,
    )
    .await;

    assert_max_weight_record(
        &program_id,
        &state_acc,
        &realm_addr,
        &mint_acc,
        &mut banks_client,
        10_000 + 3000 + 5000,
    )
    .await;

    let withdraw_inst = withdraw(
        &program_id,
        &state_acc.pubkey(),
        &vault.pubkey(),
        &mint_acc.pubkey(),
        &user_acc.pubkey(),
        &owner_acc.pubkey(),
        &token_owner_record,
        &fee_payer.pubkey(),
        5_000,
    );

    let mut transaction = Transaction::new_with_payer(&[withdraw_inst], Some(&fee_payer.pubkey()));

    transaction.sign(&[&fee_payer, &user_acc], recent_blockhash);

    banks_client
        .process_transaction(transaction)
        .await
        .expect("process withdraw");

    assert_weight_record(
        &program_id,
        &state_acc,
        &realm_addr,
        &mint_acc,
        &user_acc,
        &mut banks_client,
        5000,
    )
    .await;

    assert_max_weight_record(
        &program_id,
        &state_acc,
        &realm_addr,
        &mint_acc,
        &mut banks_client,
        5000 + 3000 + 5000,
    )
    .await;
}

async fn assert_max_weight_record(
    program_id: &Pubkey,
    state_acc: &Keypair,
    realm_addr: &Pubkey,
    mint_acc: &Keypair,
    banks_client: &mut BanksClient,
    amount: u64,
) {
    let (max_weight_record, _) = max_weight_record!(&program_id, state_acc.pubkey());

    let record = banks_client
        .get_account(max_weight_record)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(record.owner, *program_id);

    let record = MaxVoterWeightRecord::deserialize(&mut record.data.as_ref()).unwrap();
    assert_eq!(record.realm, *realm_addr);
    assert_eq!(record.governing_token_mint, mint_acc.pubkey());
    assert_eq!(record.max_voter_weight, amount);
}

async fn assert_weight_record(
    program_id: &Pubkey,
    state_acc: &Keypair,
    realm_addr: &Pubkey,
    mint_acc: &Keypair,
    user_acc: &Keypair,
    banks_client: &mut BanksClient,
    amount: u64,
) {
    let (weight_record, _) = weight_record!(&program_id, state_acc.pubkey(), user_acc.pubkey());

    let record = banks_client
        .get_account(weight_record)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(record.owner, *program_id);

    let record = VoterWeightRecord::deserialize(&mut record.data.as_ref()).unwrap();
    assert_eq!(record.realm, *realm_addr);
    assert_eq!(record.governing_token_mint, mint_acc.pubkey());
    assert_eq!(record.voter_weight, amount);
}
