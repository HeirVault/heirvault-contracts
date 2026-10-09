//! External integration tests for HeirVault.
//!
//! Everything here goes through `heirvault`'s *public* surface — the generated
//! `HeirVaultContractClient`, the re-exported value types and the documented
//! constants. No `pub(crate)` item is reachable, so these tests are the closest
//! thing to how a separate SDK, indexer or frontend will consume the contract.

use heirvault::{
    ActivationMode, HeirVaultContract, HeirVaultContractClient, HeirVaultError, VaultStatus,
    BPS_DENOMINATOR, MAX_BENEFICIARIES, MAX_GUARDIANS, MAX_PAGE_LIMIT, SCHEMA_VERSION,
};
use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{token, Address, Env};

/// Ledger seconds. `30 days` keeps the fixtures readable.
const CHECK_IN: u64 = 30 * 24 * 60 * 60;
const GRACE: u64 = 7 * 24 * 60 * 60;
const START: u64 = 1_800_000_000;
/// One whole token of a 6-decimal asset.
const UNIT: i128 = 1_000_000;

/// Build a fixture through the public ABI only.
///
/// Returns `(contract_id, asset, owner, client)`; the `Env` is owned by the
/// caller so the client's borrow stays valid for the whole test.
fn fixture(env: &Env) -> (Address, Address, Address, HeirVaultContractClient<'_>) {
    env.mock_all_auths();
    env.ledger().set_timestamp(START);

    let admin = Address::generate(env);
    let asset = env.register_stellar_asset_contract_v2(admin).address();
    let contract = env.register(HeirVaultContract, ());
    let client = HeirVaultContractClient::new(env, &contract);
    let owner = Address::generate(env);
    (contract, asset, owner, client)
}

fn mint(env: &Env, asset: &Address, to: &Address, amount: i128) {
    token::StellarAssetClient::new(env, asset).mint(to, &amount);
}

#[test]
fn published_constants_match_the_documented_contract_limits() {
    assert_eq!(BPS_DENOMINATOR, 10_000, "allocation is in basis points");
    assert_eq!(MAX_BENEFICIARIES, 10);
    assert_eq!(MAX_GUARDIANS, 5);
    assert_eq!(MAX_PAGE_LIMIT, 25);
    assert_eq!(SCHEMA_VERSION, 1);
}

#[test]
fn schema_version_is_readable_without_a_vault() {
    let env = Env::default();
    let (_, _, _, client) = fixture(&env);
    assert_eq!(client.schema_version(), SCHEMA_VERSION);
    assert_eq!(client.get_vault_count(), 0);
}

#[test]
fn an_external_caller_can_drive_the_whole_inheritance_lifecycle() {
    let env = Env::default();
    let (contract, asset, owner, client) = fixture(&env);

    let heir_a = Address::generate(&env);
    let heir_b = Address::generate(&env);

    // --- create and configure ---
    let vault_id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    client.add_beneficiary(&vault_id, &heir_a, &7_000u32);
    client.add_beneficiary(&vault_id, &heir_b, &3_000u32);
    assert_eq!(client.get_total_allocation(&vault_id), 10_000);

    // --- fund ---
    mint(&env, &asset, &owner, 1_000 * UNIT);
    client.deposit(&vault_id, &asset, &(1_000 * UNIT));

    // --- check in on time ---
    env.ledger().set_timestamp(START + 10 * 24 * 60 * 60);
    let deadline = client.check_in(&vault_id);
    assert_eq!(deadline, START + 10 * 24 * 60 * 60 + CHECK_IN);
    assert_eq!(client.get_vault_status(&vault_id), VaultStatus::Active);

    // --- miss two windows: deadline, then grace ---
    env.ledger()
        .set_timestamp(START + 10 * 24 * 60 * 60 + CHECK_IN);
    assert_eq!(
        client.get_vault_status(&vault_id),
        VaultStatus::GracePeriod,
        "the vault is overdue the instant the deadline is reached"
    );
    assert!(matches!(
        client.try_activate_vault(&vault_id),
        Err(Ok(HeirVaultError::GracePeriodActive))
    ));

    // --- release, then claim ---
    env.ledger()
        .set_timestamp(START + 10 * 24 * 60 * 60 + CHECK_IN + GRACE);
    assert!(client.get_vault(&vault_id).activation_ready);
    client.activate_vault(&vault_id);
    assert!(client.is_claimable(&vault_id));
    assert_eq!(client.get_vault_status(&vault_id), VaultStatus::Activated);

    assert_eq!(client.claim(&vault_id, &heir_a), 700 * UNIT);
    assert_eq!(client.claim(&vault_id, &heir_b), 300 * UNIT);

    // --- settled ---
    assert_eq!(client.get_vault_status(&vault_id), VaultStatus::Completed);
    assert_eq!(token::Client::new(&env, &asset).balance(&contract), 0);
    assert_eq!(
        token::Client::new(&env, &asset).balance(&heir_a),
        700 * UNIT
    );
    assert_eq!(
        token::Client::new(&env, &asset).balance(&heir_b),
        300 * UNIT
    );
}

#[test]
fn error_codes_are_stable_across_the_public_abi() {
    let env = Env::default();
    let (_, asset, owner, client) = fixture(&env);

    // Unknown vault.
    assert!(matches!(
        client.try_get_vault(&7),
        Err(Ok(HeirVaultError::VaultNotFound))
    ));

    // Invalid period.
    assert!(matches!(
        client.try_create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &0u64,
            &GRACE
        ),
        Err(Ok(HeirVaultError::InvalidPeriod))
    ));

    let vault_id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    // Zero allocation and over-100% allocation.
    let heir = Address::generate(&env);
    assert!(matches!(
        client.try_add_beneficiary(&vault_id, &heir, &0u32),
        Err(Ok(HeirVaultError::InvalidAllocation))
    ));
    client.add_beneficiary(&vault_id, &heir, &10_000u32);
    assert!(matches!(
        client.try_add_beneficiary(&vault_id, &Address::generate(&env), &1u32),
        Err(Ok(HeirVaultError::AllocationExceeded))
    ));

    // Zero deposit and mismatched asset.
    assert!(matches!(
        client.try_deposit(&vault_id, &asset, &0),
        Err(Ok(HeirVaultError::InvalidAmount))
    ));
    let other = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    mint(&env, &other, &owner, UNIT);
    assert!(matches!(
        client.try_deposit(&vault_id, &other, &UNIT),
        Err(Ok(HeirVaultError::InvalidAsset))
    ));

    // Claim before activation.
    assert!(matches!(
        client.try_claim(&vault_id, &heir),
        Err(Ok(HeirVaultError::NotActivated))
    ));

    // Paging.
    assert!(matches!(
        client.try_get_vaults_by_owner(&owner, &0, &0),
        Err(Ok(HeirVaultError::InvalidPageLimit))
    ));
}

#[test]
fn an_external_caller_can_page_an_owners_vaults() {
    let env = Env::default();
    let (_, asset, owner, client) = fixture(&env);

    for _ in 0..5 {
        client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE,
        );
    }

    let page = client.get_vaults_by_owner(&owner, &0, &2);
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.meta.total, 5);
    assert!(page.meta.has_more);
    assert_eq!(page.meta.next_offset, Some(2));

    let last = client.get_vaults_by_owner(&owner, &4, &2);
    assert_eq!(last.items.len(), 1);
    assert!(!last.meta.has_more);
    assert_eq!(last.meta.next_offset, None);
}

#[test]
fn a_guardian_can_release_a_vault_without_waiting_for_the_clock() {
    let env = Env::default();
    let (_, asset, owner, client) = fixture(&env);

    let heir = Address::generate(&env);
    let guardian_one = Address::generate(&env);
    let guardian_two = Address::generate(&env);

    let vault_id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::GuardianApproval,
        &CHECK_IN,
        &GRACE,
    );
    client.add_beneficiary(&vault_id, &heir, &10_000u32);
    client.add_guardian(&vault_id, &guardian_one);
    client.add_guardian(&vault_id, &guardian_two);
    client.set_guardian_threshold(&vault_id, &2u32);
    mint(&env, &asset, &owner, 100 * UNIT);
    client.deposit(&vault_id, &asset, &(100 * UNIT));

    // Long before the check-in deadline, one vote is not enough.
    client.guardian_approve(&vault_id, &guardian_one);
    let status = client.get_guardian_status(&vault_id);
    assert_eq!(status.approvals, 1);
    assert!(!status.threshold_met);
    assert!(matches!(
        client.try_activate_vault(&vault_id),
        Err(Ok(HeirVaultError::NotEligible))
    ));

    // The second vote is.
    client.guardian_approve(&vault_id, &guardian_two);
    assert!(client.get_guardian_status(&vault_id).threshold_met);
    client.activate_vault(&vault_id);
    assert_eq!(client.claim(&vault_id, &heir), 100 * UNIT);
}

#[test]
fn a_beneficiary_can_read_its_own_position_before_claiming() {
    let env = Env::default();
    let (_, asset, owner, client) = fixture(&env);

    let heir = Address::generate(&env);
    let vault_id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    client.add_beneficiary(&vault_id, &heir, &2_500u32);
    client.add_beneficiary(&vault_id, &Address::generate(&env), &7_500u32);
    mint(&env, &asset, &owner, 400 * UNIT);
    client.deposit(&vault_id, &asset, &(400 * UNIT));
    env.ledger().set_timestamp(START + CHECK_IN + GRACE);
    client.activate_vault(&vault_id);

    let position = client.get_claim(&vault_id, &heir);
    assert_eq!(position.allocation_bps, 2_500);
    assert_eq!(position.entitlement, 100 * UNIT);
    assert_eq!(position.claimed_amount, 0);
    assert!(!position.claimed);

    let page = client.get_claims(&vault_id, &0, &10);
    assert_eq!(page.meta.total, 2);
    assert_eq!(page.items.get(0).unwrap().entitlement, 100 * UNIT);
    assert_eq!(page.items.get(1).unwrap().entitlement, 300 * UNIT);

    assert_eq!(client.claim(&vault_id, &heir), 100 * UNIT);
    assert!(client.get_claim(&vault_id, &heir).claimed);
    assert_eq!(client.get_undistributed_balance(&vault_id), 300 * UNIT);
}

#[test]
fn a_cancelled_vault_returns_everything_to_the_owner() {
    let env = Env::default();
    let (contract, asset, owner, client) = fixture(&env);

    let heir = Address::generate(&env);
    let vault_id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    client.add_beneficiary(&vault_id, &heir, &10_000u32);
    mint(&env, &asset, &owner, 250 * UNIT);
    client.deposit(&vault_id, &asset, &(250 * UNIT));

    client.cancel_vault(&vault_id);
    assert_eq!(client.get_vault_status(&vault_id), VaultStatus::Cancelled);
    client.withdraw(&vault_id, &(250 * UNIT));

    assert_eq!(token::Client::new(&env, &asset).balance(&owner), 250 * UNIT);
    assert_eq!(token::Client::new(&env, &asset).balance(&contract), 0);

    // A cancelled vault can never be activated or claimed from.
    env.ledger().set_timestamp(START + CHECK_IN + GRACE);
    assert!(matches!(
        client.try_activate_vault(&vault_id),
        Err(Ok(HeirVaultError::VaultCancelled))
    ));
    assert!(matches!(
        client.try_claim(&vault_id, &heir),
        Err(Ok(HeirVaultError::VaultCancelled))
    ));
}

/// The fixtures in this file are constructed identically each time; this is a
/// small guard that the `Client` type really is borrow-based and reusable.
#[test]
fn the_client_is_cheap_to_construct_for_each_vault() {
    let env = Env::default();
    let (contract, asset, owner, client) = fixture(&env);
    let _ = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    // A second handle onto the same contract behaves identically.
    let same = HeirVaultContractClient::new(&env, &contract);
    assert_eq!(same.get_vault_count(), 1);
    let info = same.get_vault(&1);
    assert_eq!(info.vault.owner, owner);
    assert_eq!(info.grace_end, START + CHECK_IN + GRACE);
}
