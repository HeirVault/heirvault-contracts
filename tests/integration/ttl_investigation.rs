use heirvault::storage::DataKey;
use heirvault::{ActivationMode, HeirVaultContract, HeirVaultContractClient};
use soroban_sdk::testutils::storage::{Instance as _, Persistent as _};
use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{token, Address, Env};

const START_TIME: u64 = 1_800_000_000;
const START_LEDGER: u32 = 100_000;
const LEDGERS_PER_DAY: u32 = 17_280;
const EXTEND_TO_LEDGERS: u32 = 90 * LEDGERS_PER_DAY; // 1,555,200

fn setup_test_vault(
    env: &Env,
    check_in_days: u64,
) -> (
    Address,
    Address,
    Address,
    Address,
    HeirVaultContractClient<'_>,
    u64,
) {
    env.mock_all_auths();
    env.ledger().set_timestamp(START_TIME);
    env.ledger().set_sequence_number(START_LEDGER);

    let admin = Address::generate(env);
    let asset = env.register_stellar_asset_contract_v2(admin).address();
    let contract_id = env.register(HeirVaultContract, ());
    let client = HeirVaultContractClient::new(env, &contract_id);
    let owner = Address::generate(env);
    let heir = Address::generate(env);

    let vault_id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &(check_in_days * 86_400),
        &(30 * 86_400),
    );

    client.add_beneficiary(&vault_id, &heir, &10_000);

    token::StellarAssetClient::new(env, &asset).mint(&owner, &1_000_000_000);
    client.deposit(&vault_id, &asset, &1_000_000_000);

    (contract_id, asset, owner, heir, client, vault_id)
}

/// 1. Verify initial TTL assignment across all HeirVault persistent and instance entries.
#[test]
fn test_initial_storage_ttls() {
    let env = Env::default();
    let (contract_id, _asset, owner, heir, _client, vault_id) = setup_test_vault(&env, 90);

    env.as_contract(&contract_id, || {
        let vault_key = DataKey::Vault(vault_id);
        let owner_count_key = DataKey::OwnerVaultCount(owner.clone());
        let owner_slot_key = DataKey::OwnerVault(owner.clone(), 0);
        let heir_count_key = DataKey::BeneficiaryVaultCount(heir.clone());
        let heir_slot_key = DataKey::BeneficiaryVault(heir.clone(), 0);

        let vault_ttl = env.storage().persistent().get_ttl(&vault_key);
        let owner_count_ttl = env.storage().persistent().get_ttl(&owner_count_key);
        let owner_slot_ttl = env.storage().persistent().get_ttl(&owner_slot_key);
        let heir_count_ttl = env.storage().persistent().get_ttl(&heir_count_key);
        let heir_slot_ttl = env.storage().persistent().get_ttl(&heir_slot_key);
        let instance_ttl = env.storage().instance().get_ttl();

        assert_eq!(
            vault_ttl, EXTEND_TO_LEDGERS,
            "Vault record initial TTL should be 90 days"
        );
        assert_eq!(
            owner_count_ttl, EXTEND_TO_LEDGERS,
            "Owner count initial TTL should be 90 days"
        );
        assert_eq!(
            owner_slot_ttl, EXTEND_TO_LEDGERS,
            "Owner slot initial TTL should be 90 days"
        );
        assert_eq!(
            heir_count_ttl, EXTEND_TO_LEDGERS,
            "Heir count initial TTL should be 90 days"
        );
        assert_eq!(
            heir_slot_ttl, EXTEND_TO_LEDGERS,
            "Heir slot initial TTL should be 90 days"
        );
        assert_eq!(
            instance_ttl, EXTEND_TO_LEDGERS,
            "Instance initial TTL should be 90 days"
        );
    });
}

/// 2. Verify that an unauthenticated public read (keeper ping) extends vault record TTL once below threshold.
#[test]
fn test_keeper_ping_extends_vault_ttl_when_below_threshold() {
    let env = Env::default();
    let (contract_id, _asset, _owner, _heir, client, vault_id) = setup_test_vault(&env, 90);

    // Advance 65 days so remaining TTL drops below 30-day threshold (518,400 ledgers)
    env.ledger()
        .set_sequence_number(START_LEDGER + 65 * LEDGERS_PER_DAY);

    // An unauthenticated party (no auth mock) calls get_vault (simulating pingVault)
    let info = client.get_vault(&vault_id);
    assert_eq!(info.vault.id, vault_id);

    env.as_contract(&contract_id, || {
        let vault_key = DataKey::Vault(vault_id);
        let ttl = env.storage().persistent().get_ttl(&vault_key);
        assert_eq!(
            ttl, EXTEND_TO_LEDGERS,
            "An unauthenticated public read successfully extends the vault TTL to 90 days"
        );
    });
}

/// 3. Verification: get_vault refreshes BOTH the vault record AND the contract instance TTL.
#[test]
fn test_keeper_ping_refreshes_both_vault_and_instance_ttl() {
    let env = Env::default();
    let (contract_id, _asset, _owner, _heir, client, vault_id) = setup_test_vault(&env, 365);

    // Call get_vault at day 60 (below 30-day threshold)
    env.ledger()
        .set_sequence_number(START_LEDGER + 60 * LEDGERS_PER_DAY);
    let _ = client.get_vault(&vault_id);

    env.as_contract(&contract_id, || {
        let vault_key = DataKey::Vault(vault_id);
        let vault_ttl = env.storage().persistent().get_ttl(&vault_key);
        let instance_ttl = env.storage().instance().get_ttl();

        // Vault record TTL was refreshed out to 90 days (1,555,200)
        assert_eq!(
            vault_ttl, EXTEND_TO_LEDGERS,
            "Vault record TTL was refreshed"
        );

        // Contract instance TTL is now also refreshed because get_vault calls touch_instance!
        assert_eq!(
            instance_ttl, EXTEND_TO_LEDGERS,
            "Contract instance TTL must be refreshed when get_vault is invoked!"
        );
    });
}

/// 4. Verification: Regular keeper pings successfully prevent contract instance archival.
#[test]
fn test_keeper_ping_successfully_prevents_instance_archival() {
    let env = Env::default();
    let (_contract_id, _asset, _owner, _heir, client, vault_id) = setup_test_vault(&env, 365);

    // Keeper pings at day 30 and day 60
    env.ledger()
        .set_sequence_number(START_LEDGER + 30 * LEDGERS_PER_DAY);
    let _ = client.get_vault(&vault_id);

    env.ledger()
        .set_sequence_number(START_LEDGER + 60 * LEDGERS_PER_DAY);
    let _ = client.get_vault(&vault_id);

    // At day 91, the contract instance TTL is still active because keeper ping touched the instance!
    env.ledger()
        .set_sequence_number(START_LEDGER + 91 * LEDGERS_PER_DAY);

    // This call succeeds without archival panic!
    let info = client.get_vault(&vault_id);
    assert_eq!(info.vault.id, vault_id);
}

/// 5. Flaw 2 (Index Decay Asymmetry): Even if contract instance is kept alive externally,
///    index slots (OwnerVault and BeneficiaryVault) expire after 90 days because get_vault never touches them.
///    Any attempt to read the owner's vaults aborts with storage error due to accessing an archived key.
#[test]
#[should_panic(expected = "Accessed contract data key key that has been archived")]
fn test_index_decay_asymmetry_panics_on_archived_owner_index() {
    let env = Env::default();
    let (contract_id, _asset, owner, _heir, client, vault_id) = setup_test_vault(&env, 365);

    // Simulate keeper extending instance TTL and calling get_vault every 60 days for 180 days
    for cycle in 1..=3 {
        let current_ledger = START_LEDGER + cycle * 60 * LEDGERS_PER_DAY;
        env.ledger().set_sequence_number(current_ledger);

        env.as_contract(&contract_id, || {
            env.storage()
                .instance()
                .extend_ttl(600_000, EXTEND_TO_LEDGERS);
        });

        let _ = client.get_vault(&vault_id);
    }

    // At day 180, vault is alive, but OwnerVault(owner, 0) is archived.
    // get_by_owner tries to read OwnerVault(owner, 0) and panics on archived key.
    let _ = client.get_vaults_by_owner(&owner, &0, &10);
}

/// 6. Dormant Vault Expiration: A vault with a 1-year or 5-year check-in period left untouched
///    for >90 days has its persistent vault entry archived.
#[test]
#[should_panic(expected = "Accessed contract instance key that has been archived")]
fn test_dormant_vault_storage_archived_after_91_days() {
    let env = Env::default();
    let (_contract_id, _asset, _owner, _heir, client, vault_id) = setup_test_vault(&env, 5 * 365);

    // Advance 91 days with no transactions
    env.ledger()
        .set_sequence_number(START_LEDGER + 91 * LEDGERS_PER_DAY);

    // Owner or beneficiary attempts to access the vault -> fails due to archival
    let _ = client.get_vault(&vault_id);
}
