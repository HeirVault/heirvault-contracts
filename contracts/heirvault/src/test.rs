//! HeirVault automated test suite.
//!
//! Layout:
//!
//! * **Harness** — fixtures and assertion macros shared by every test.
//! * **Creation / deposits** — parameter validation and real SEP-41 movement.
//! * **Beneficiaries** — the allocation invariant at and around 100%.
//! * **Check-in and grace period** — every timestamp boundary, explicitly.
//! * **Activation** — each activation mode, plus the "too early" paths.
//! * **Claims** — distribution correctness, rounding and double claims.
//! * **Cancellation / withdrawal** — the owner's exit path.
//! * **Guardians** — M-of-N, vote reset and anti-lockout.
//! * **Pagination** — page metadata, clamping and limit validation.
//! * **Authorization** — every privileged entry point against a caller that is
//!   not the address the contract requires.
//! * **Invariants** — properties that must hold across randomized sequences.

extern crate std;

use std::vec::Vec as StdVec;

use soroban_sdk::testutils::{Address as _, Events, Ledger as _, MockAuth, MockAuthInvoke};
use soroban_sdk::{token, Address, Env, IntoVal, Symbol, TryFromVal, Val, Vec};

use crate::{
    ActivationMode, HeirVaultContract, HeirVaultContractClient, HeirVaultError, VaultStatus,
};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A 30 day check-in period, expressed the way a real deployment would set it.
const CHECK_IN: u64 = 30 * 24 * 60 * 60;
/// A 7 day grace period.
const GRACE: u64 = 7 * 24 * 60 * 60;
/// One second, used to probe exact boundaries.
const ONE: u64 = 1;
/// Arbitrary but fixed clock origin, so every expected timestamp is a literal.
const START: u64 = 1_700_000_000;
/// Base units of a 6-decimal asset (1.0 token).
const UNIT: i128 = 1_000_000;

/// Assert a call failed with a specific contract error.
macro_rules! assert_contract_error {
    ($call:expr, $expected:expr) => {
        match $call {
            Err(Ok(err)) => assert_eq!(err, $expected, "wrong contract error returned"),
            other => panic!("expected contract error {:?}, got {:?}", $expected, other),
        }
    };
}

/// Assert a call was rejected for any reason (used for authorization failures,
/// which surface as host aborts rather than contract errors).
macro_rules! assert_rejected {
    ($call:expr) => {
        assert!($call.is_err(), "expected the call to be rejected")
    };
}

/// Register a Stellar Asset Contract and a fresh HeirVault instance.
fn deploy(env: &Env) -> (Address, Address) {
    let admin = Address::generate(env);
    let asset = env.register_stellar_asset_contract_v2(admin).address();
    let contract = env.register(HeirVaultContract, ());
    (contract, asset)
}

/// Standard fixture: mocked auth, a fixed clock, a token and a contract.
fn world(env: &Env) -> (Address, Address, Address, HeirVaultContractClient<'_>) {
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let (contract, asset) = deploy(env);
    let client = HeirVaultContractClient::new(env, &contract);
    let owner = Address::generate(env);
    (contract, asset, owner, client)
}

/// A vault with a single 100% beneficiary, funded with `amount`.
fn funded_vault(
    env: &Env,
    mode: ActivationMode,
    amount: i128,
) -> (
    u64,
    Address,
    Address,
    Address,
    Address,
    HeirVaultContractClient<'_>,
) {
    let (contract, asset, owner, client) = world(env);
    let id = client.create_vault(&owner, &asset, &mode, &CHECK_IN, &GRACE);
    let heir = Address::generate(env);
    client.add_beneficiary(&id, &heir, &10_000u32);
    mint(env, &asset, &owner, amount);
    client.deposit(&id, &asset, &amount);
    (id, contract, asset, owner, heir, client)
}

/// Mint test tokens to an address.
fn mint(env: &Env, asset: &Address, to: &Address, amount: i128) {
    token::StellarAssetClient::new(env, asset).mint(to, &amount);
}

/// Token balance of an address.
fn balance(env: &Env, asset: &Address, who: &Address) -> i128 {
    token::Client::new(env, asset).balance(who)
}

/// Advance the ledger clock by `seconds`.
fn advance(env: &Env, seconds: u64) {
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + seconds);
}

/// Disable auth mocking, so every `require_auth` in the next call fails.
fn refuse_all_auth(env: &Env) {
    env.set_auths(&[]);
}

/// Replace auth mocking with a single entry for `address`.
///
/// Installing exactly one entry *disables* blanket mocking (see
/// `Env::set_auths`), so any other address the contract asks to authorize will
/// fail. That is what makes these tests meaningful: the contract must require
/// the address it claims to require, not merely require *some* authorization.
fn authorize_only(env: &Env, address: &Address, contract: &Address, fn_name: &str, args: Vec<Val>) {
    let invoke = MockAuthInvoke {
        contract,
        fn_name,
        args,
        sub_invokes: &[],
    };
    env.mock_auths(&[MockAuth {
        address,
        invoke: &invoke,
    }]);
}

/// Drain the event names published since the last drain.
///
/// The test host reports events per contract invocation, so the buffer must be
/// read immediately after the call that emits them. [`capture!`] and `step!`
/// (inside the lifecycle test) do exactly that.
fn drain_events(env: &Env) -> StdVec<Symbol> {
    let mut names = StdVec::new();
    for (_, topics, _) in env.events().all().iter() {
        if let Some(first) = topics.get(0) {
            if let Ok(name) = Symbol::try_from_val(env, &first) {
                names.push(name);
            }
        }
    }
    names
}

/// `true` when a captured event list contains the event named `name`.
fn has_event(env: &Env, names: &[Symbol], name: &str) -> bool {
    names.contains(&Symbol::new(env, name))
}

/// Run `$call` and capture the event names it published.
///
/// Yields `(return_value, event_names)`.
macro_rules! capture {
    ($env:expr, $call:expr) => {{
        let value = $call;
        (value, drain_events($env))
    }};
}

// ---------------------------------------------------------------------------
// Vault creation
// ---------------------------------------------------------------------------

#[test]
fn create_vault_stores_expected_initial_state() {
    let env = Env::default();
    env.ledger().set_timestamp(START);
    let (contract, asset, owner, client) = world(&env);

    let (id, events) = capture!(
        &env,
        client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE
        )
    );
    assert_eq!(id, 1, "ids start at 1 so 0 can mean 'no vault'");
    assert!(has_event(&env, &events, "vault_created"));

    let info = client.get_vault(&id);
    assert_eq!(info.vault.owner, owner);
    assert_eq!(info.vault.asset, asset);
    assert_eq!(info.vault.balance, 0, "creation must not require funding");
    assert_eq!(info.vault.status, VaultStatus::Active);
    assert_eq!(info.effective_status, VaultStatus::Active);
    assert_eq!(info.vault.check_in_period, CHECK_IN);
    assert_eq!(info.vault.grace_period, GRACE);
    assert_eq!(info.vault.last_check_in, START);
    assert_eq!(info.deadline, START + CHECK_IN);
    assert_eq!(info.grace_end, START + CHECK_IN + GRACE);
    assert_eq!(info.total_allocation_bps, 0);
    assert_eq!(info.guardian_threshold, 0);
    assert_eq!(info.guardian_approvals, 0);
    assert!(!info.activation_ready, "an unfunded vault cannot activate");
    assert_eq!(info.vault.created_at, START);
    assert_eq!(
        info.vault.schema_version,
        crate::SCHEMA_VERSION,
        "every vault stamps the layout version it was written with"
    );
    assert_eq!(contract, client.address);
}

#[test]
fn create_vault_emits_ids_without_collision() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);

    let a = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    let b = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    let c = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::GuardianApproval,
        &CHECK_IN,
        &GRACE,
    );

    assert_eq!((a, b, c), (1, 2, 3));
    assert_eq!(client.get_vault_count(), 3);
    assert_eq!(client.get_vault(&a).vault.id, a);
    assert_eq!(
        client.get_vault(&c).vault.activation_mode,
        ActivationMode::GuardianApproval
    );
}

#[test]
fn create_vault_rejects_check_in_period_out_of_range() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);

    for bad in [0u64, 1, crate::MIN_CHECK_IN_PERIOD - 1] {
        assert_contract_error!(
            client.try_create_vault(&owner, &asset, &ActivationMode::MissedCheckIn, &bad, &GRACE),
            HeirVaultError::InvalidPeriod
        );
    }
    for bad in [crate::MAX_CHECK_IN_PERIOD + 1, u64::MAX] {
        assert_contract_error!(
            client.try_create_vault(&owner, &asset, &ActivationMode::MissedCheckIn, &bad, &GRACE),
            HeirVaultError::InvalidPeriod
        );
    }
    // The documented bounds themselves are accepted.
    client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &crate::MIN_CHECK_IN_PERIOD,
        &GRACE,
    );
    client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &crate::MAX_CHECK_IN_PERIOD,
        &GRACE,
    );
}

#[test]
fn create_vault_rejects_grace_period_out_of_range() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);

    for bad in [0u64, 1, crate::MIN_GRACE_PERIOD - 1] {
        assert_contract_error!(
            client.try_create_vault(
                &owner,
                &asset,
                &ActivationMode::MissedCheckIn,
                &CHECK_IN,
                &bad
            ),
            HeirVaultError::InvalidPeriod
        );
    }
    for bad in [crate::MAX_GRACE_PERIOD + 1, u64::MAX] {
        assert_contract_error!(
            client.try_create_vault(
                &owner,
                &asset,
                &ActivationMode::MissedCheckIn,
                &CHECK_IN,
                &bad
            ),
            HeirVaultError::InvalidPeriod
        );
    }
    client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &crate::MIN_GRACE_PERIOD,
    );
    client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &crate::MAX_GRACE_PERIOD,
    );
}

#[test]
fn create_vault_rejects_asset_that_is_not_a_token() {
    let env = Env::default();
    let (_, _, owner, client) = world(&env);

    // A plain account address answers no `decimals()` call.
    let not_a_token = Address::generate(&env);
    assert_contract_error!(
        client.try_create_vault(
            &owner,
            &not_a_token,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE
        ),
        HeirVaultError::InvalidAsset
    );
}

#[test]
fn create_vault_rejects_the_contract_itself_as_asset() {
    let env = Env::default();
    let (contract, _, owner, client) = world(&env);

    assert_contract_error!(
        client.try_create_vault(
            &owner,
            &contract,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE
        ),
        HeirVaultError::InvalidAsset
    );
}

#[test]
fn create_vault_requires_owner_authentication() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    refuse_all_auth(&env);

    assert_rejected!(client.try_create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE
    ));
}

#[test]
fn unknown_vault_id_is_reported() {
    let env = Env::default();
    let (_, _, _, client) = world(&env);
    assert_contract_error!(client.try_get_vault(&404), HeirVaultError::VaultNotFound);
    assert_contract_error!(
        client.try_get_vault_status(&404),
        HeirVaultError::VaultNotFound
    );
}

// ---------------------------------------------------------------------------
// Deposits
// ---------------------------------------------------------------------------

#[test]
fn deposit_moves_real_tokens_and_tracks_balance() {
    let env = Env::default();
    let (contract, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    mint(&env, &asset, &owner, 1_000 * UNIT);
    assert_eq!(balance(&env, &asset, &owner), 1_000 * UNIT);

    let (_, events) = capture!(&env, client.deposit(&id, &asset, &(250 * UNIT)));
    assert!(has_event(&env, &events, "deposit_made"));

    assert_eq!(
        balance(&env, &asset, &owner),
        750 * UNIT,
        "owner must be debited"
    );
    assert_eq!(
        balance(&env, &asset, &contract),
        250 * UNIT,
        "the vault contract must actually hold the tokens"
    );
    assert_eq!(
        client.get_vault(&id).vault.balance,
        250 * UNIT,
        "internal accounting must match the transfer"
    );
}

#[test]
fn deposit_accumulates_across_multiple_calls() {
    let env = Env::default();
    let (contract, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    mint(&env, &asset, &owner, 1_000 * UNIT);

    client.deposit(&id, &asset, &(100 * UNIT));
    client.deposit(&id, &asset, &(200 * UNIT));
    client.deposit(&id, &asset, &UNIT);

    assert_eq!(client.get_vault(&id).vault.balance, 301 * UNIT);
    assert_eq!(balance(&env, &asset, &contract), 301 * UNIT);
}

#[test]
fn deposit_rejects_non_positive_amounts() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    mint(&env, &asset, &owner, 100 * UNIT);

    assert_contract_error!(
        client.try_deposit(&id, &asset, &0),
        HeirVaultError::InvalidAmount
    );
    assert_contract_error!(
        client.try_deposit(&id, &asset, &-1),
        HeirVaultError::InvalidAmount
    );
    assert_eq!(client.get_vault(&id).vault.balance, 0);
}

#[test]
fn deposit_rejects_mismatched_asset() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    // A second, unrelated token that the vault was not configured for.
    let other_admin = Address::generate(&env);
    let other = env
        .register_stellar_asset_contract_v2(other_admin)
        .address();
    mint(&env, &other, &owner, 100 * UNIT);

    assert_contract_error!(
        client.try_deposit(&id, &other, &(100 * UNIT)),
        HeirVaultError::InvalidAsset
    );
    assert_eq!(
        balance(&env, &other, &owner),
        100 * UNIT,
        "the wrong asset must not be touched at all"
    );
}

#[test]
fn deposit_requires_owner_authentication() {
    let env = Env::default();
    let (contract, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    mint(&env, &asset, &owner, 100 * UNIT);

    let attacker = Address::generate(&env);
    authorize_only(
        &env,
        &attacker,
        &contract,
        "deposit",
        soroban_sdk::vec![
            &env,
            id.into_val(&env),
            asset.to_val(),
            (100 * UNIT).into_val(&env)
        ],
    );

    assert_rejected!(client.try_deposit(&id, &asset, &(100 * UNIT)));
    assert_eq!(client.get_vault(&id).vault.balance, 0);
}

#[test]
fn deposit_is_rejected_once_the_vault_is_activated() {
    let env = Env::default();
    let (id, _, asset, owner, _, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    mint(&env, &asset, &owner, 10 * UNIT);
    assert_contract_error!(
        client.try_deposit(&id, &asset, &(10 * UNIT)),
        HeirVaultError::AlreadyActivated
    );
    assert_eq!(client.get_vault(&id).vault.balance, 100 * UNIT);
}

#[test]
fn deposit_is_rejected_once_the_vault_is_cancelled() {
    let env = Env::default();
    let (id, _, asset, owner, _, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.cancel_vault(&id);

    mint(&env, &asset, &owner, 10 * UNIT);
    assert_contract_error!(
        client.try_deposit(&id, &asset, &(10 * UNIT)),
        HeirVaultError::VaultCancelled
    );
}

#[test]
fn deposit_is_allowed_during_the_grace_period() {
    let env = Env::default();
    let (id, _, asset, owner, _, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + ONE); // deadline passed, grace period running

    mint(&env, &asset, &owner, 10 * UNIT);
    client.deposit(&id, &asset, &(10 * UNIT));
    assert_eq!(client.get_vault(&id).vault.balance, 110 * UNIT);
}

// ---------------------------------------------------------------------------
// Beneficiaries and the allocation invariant
// ---------------------------------------------------------------------------

#[test]
fn add_beneficiary_records_the_slot() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    let heir = Address::generate(&env);

    let (_, events) = capture!(&env, client.add_beneficiary(&id, &heir, &4_000u32));
    assert!(has_event(&env, &events, "beneficiary_added"));

    let page = client.get_beneficiaries(&id, &0, &10);
    assert_eq!(page.meta.total, 1);
    assert_eq!(page.items.get(0).unwrap().address, heir);
    assert_eq!(page.items.get(0).unwrap().allocation_bps, 4_000);
    assert!(page.items.get(0).unwrap().active);
    assert!(!page.items.get(0).unwrap().claimed);
    assert_eq!(client.get_total_allocation(&id), 4_000);
}

#[test]
fn add_beneficiary_rejects_zero_allocation() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    let heir = Address::generate(&env);

    assert_contract_error!(
        client.try_add_beneficiary(&id, &heir, &0u32),
        HeirVaultError::InvalidAllocation
    );
}

#[test]
fn add_beneficiary_rejects_allocation_above_one_hundred_percent() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    let heir = Address::generate(&env);

    assert_contract_error!(
        client.try_add_beneficiary(&id, &heir, &10_001u32),
        HeirVaultError::InvalidAllocation
    );
}

#[test]
fn add_beneficiary_rejects_duplicate_addresses() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    let heir = Address::generate(&env);

    client.add_beneficiary(&id, &heir, &3_000u32);
    assert_contract_error!(
        client.try_add_beneficiary(&id, &heir, &2_000u32),
        HeirVaultError::DuplicateBeneficiary
    );
    assert_eq!(client.get_total_allocation(&id), 3_000);
}

#[test]
fn add_beneficiary_rejects_the_owner_and_the_contract() {
    let env = Env::default();
    let (contract, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    assert_contract_error!(
        client.try_add_beneficiary(&id, &owner, &1_000u32),
        HeirVaultError::InvalidAddress
    );
    assert_contract_error!(
        client.try_add_beneficiary(&id, &contract, &1_000u32),
        HeirVaultError::InvalidAddress
    );
}

#[test]
fn add_beneficiary_enforces_the_slot_cap() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    for _ in 0..crate::MAX_BENEFICIARIES {
        client.add_beneficiary(&id, &Address::generate(&env), &1_000u32);
    }
    assert_eq!(client.get_total_allocation(&id), 10_000);

    assert_contract_error!(
        client.try_add_beneficiary(&id, &Address::generate(&env), &1u32),
        HeirVaultError::TooManyBeneficiaries
    );

    // Slots are retained rather than compacted, so once the cap is reached the
    // only address that fits is one that already has a (retired) slot. This is
    // the documented lifetime cap: at most MAX_BENEFICIARIES distinct addresses.
    let retired = client.get_active_beneficiaries(&id).get(0).unwrap().address;
    client.remove_beneficiary(&id, &retired);
    assert_contract_error!(
        client.try_add_beneficiary(&id, &Address::generate(&env), &1_000u32),
        HeirVaultError::TooManyBeneficiaries
    );
    client.add_beneficiary(&id, &retired, &1_000u32);
    assert_eq!(
        client.get_active_beneficiaries(&id).len(),
        crate::MAX_BENEFICIARIES
    );
}

#[test]
fn allocation_may_total_less_than_one_hundred_percent_while_editing() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    client.add_beneficiary(&id, &Address::generate(&env), &1u32);
    assert_eq!(client.get_total_allocation(&id), 1);

    client.add_beneficiary(&id, &Address::generate(&env), &9_998u32);
    assert_eq!(client.get_total_allocation(&id), 9_999);
}

#[test]
fn allocation_may_total_exactly_one_hundred_percent() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    client.add_beneficiary(&id, &Address::generate(&env), &9_999u32);
    client.add_beneficiary(&id, &Address::generate(&env), &1u32);
    assert_eq!(client.get_total_allocation(&id), 10_000);
}

#[test]
fn allocation_may_not_exceed_one_hundred_percent() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    client.add_beneficiary(&id, &Address::generate(&env), &5_000u32);
    client.add_beneficiary(&id, &Address::generate(&env), &5_000u32);
    assert_contract_error!(
        client.try_add_beneficiary(&id, &Address::generate(&env), &1u32),
        HeirVaultError::AllocationExceeded
    );
    assert_eq!(client.get_total_allocation(&id), 10_000);
}

#[test]
fn removing_a_beneficiary_frees_its_allocation() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    let heir = Address::generate(&env);

    client.add_beneficiary(&id, &heir, &6_000u32);
    client.add_beneficiary(&id, &Address::generate(&env), &4_000u32);

    let (_, events) = capture!(&env, client.remove_beneficiary(&id, &heir));
    assert!(has_event(&env, &events, "beneficiary_removed"));
    assert_eq!(client.get_total_allocation(&id), 4_000);

    // The vacated share is immediately reusable.
    let replacement = Address::generate(&env);
    client.add_beneficiary(&id, &replacement, &6_000u32);
    assert_eq!(client.get_total_allocation(&id), 10_000);
}

#[test]
fn removing_a_beneficiary_retains_an_inactive_slot() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    let heir = Address::generate(&env);

    client.add_beneficiary(&id, &heir, &5_000u32);
    client.remove_beneficiary(&id, &heir);

    let page = client.get_beneficiaries(&id, &0, &10);
    assert_eq!(page.meta.total, 1, "the slot is kept for on-chain history");
    assert!(!page.items.get(0).unwrap().active);
    assert_eq!(page.items.get(0).unwrap().allocation_bps, 0);
    assert_eq!(client.get_active_beneficiaries(&id).len(), 0);
}

#[test]
fn removing_an_unknown_beneficiary_is_rejected() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    assert_contract_error!(
        client.try_remove_beneficiary(&id, &Address::generate(&env)),
        HeirVaultError::InvalidBeneficiary
    );
}

#[test]
fn re_adding_a_removed_beneficiary_reactivates_its_slot() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    let heir = Address::generate(&env);

    client.add_beneficiary(&id, &heir, &5_000u32);
    client.remove_beneficiary(&id, &heir);
    client.add_beneficiary(&id, &heir, &7_000u32);

    let page = client.get_beneficiaries(&id, &0, &10);
    assert_eq!(page.meta.total, 1, "the address must never appear twice");
    assert!(page.items.get(0).unwrap().active);
    assert_eq!(page.items.get(0).unwrap().allocation_bps, 7_000);
}

#[test]
fn update_beneficiary_changes_the_share() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    let heir = Address::generate(&env);

    client.add_beneficiary(&id, &heir, &5_000u32);
    let (_, events) = capture!(&env, client.update_beneficiary(&id, &heir, &8_000u32));
    assert!(has_event(&env, &events, "beneficiary_updated"));

    assert_eq!(client.get_total_allocation(&id), 8_000);
    assert_eq!(
        client
            .get_beneficiaries(&id, &0, &1)
            .items
            .get(0)
            .unwrap()
            .allocation_bps,
        8_000
    );
}

#[test]
fn update_beneficiary_cannot_exceed_one_hundred_percent_total() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    let first = Address::generate(&env);

    client.add_beneficiary(&id, &first, &5_000u32);
    client.add_beneficiary(&id, &Address::generate(&env), &5_000u32);

    assert_contract_error!(
        client.try_update_beneficiary(&id, &first, &5_001u32),
        HeirVaultError::AllocationExceeded
    );
    assert_eq!(client.get_total_allocation(&id), 10_000);
}

#[test]
fn update_beneficiary_rejects_zero_and_over_one_hundred_percent() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    let heir = Address::generate(&env);
    client.add_beneficiary(&id, &heir, &5_000u32);

    assert_contract_error!(
        client.try_update_beneficiary(&id, &heir, &0u32),
        HeirVaultError::InvalidAllocation
    );
    assert_contract_error!(
        client.try_update_beneficiary(&id, &heir, &10_001u32),
        HeirVaultError::InvalidAllocation
    );
}

#[test]
fn update_beneficiary_rejects_removed_or_unknown_addresses() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    let heir = Address::generate(&env);

    client.add_beneficiary(&id, &heir, &5_000u32);
    client.remove_beneficiary(&id, &heir);

    assert_contract_error!(
        client.try_update_beneficiary(&id, &heir, &1_000u32),
        HeirVaultError::InvalidBeneficiary
    );
}

#[test]
fn beneficiary_edits_are_frozen_after_activation() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    let extra = Address::generate(&env);
    assert_contract_error!(
        client.try_add_beneficiary(&id, &extra, &1u32),
        HeirVaultError::AlreadyActivated
    );
    assert_contract_error!(
        client.try_update_beneficiary(&id, &extra, &1u32),
        HeirVaultError::AlreadyActivated
    );
    assert_contract_error!(
        client.try_remove_beneficiary(&id, &extra),
        HeirVaultError::AlreadyActivated
    );
}

#[test]
fn beneficiary_edits_are_frozen_after_cancellation() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.cancel_vault(&id);

    assert_contract_error!(
        client.try_add_beneficiary(&id, &Address::generate(&env), &1u32),
        HeirVaultError::VaultCancelled
    );
}

// ---------------------------------------------------------------------------
// Check-in and the deadline/grace-period boundaries
// ---------------------------------------------------------------------------

#[test]
fn check_in_restarts_the_clock() {
    let env = Env::default();
    let (id, _, _, owner, _, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    let first_deadline = client.get_deadlines(&id).deadline;
    assert_eq!(first_deadline, START + CHECK_IN);

    advance(&env, 10 * 24 * 60 * 60); // well inside the period
    let (new_deadline, events) = capture!(&env, client.check_in(&id));
    assert!(has_event(&env, &events, "check_in_completed"));

    assert_eq!(new_deadline, START + 10 * 24 * 60 * 60 + CHECK_IN);
    assert_eq!(client.get_vault(&id).vault.status, VaultStatus::Active);
    let _ = owner;
}

#[test]
fn check_in_requires_owner_authentication() {
    let env = Env::default();
    let (id, contract, _, owner, _, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    let attacker = Address::generate(&env);
    authorize_only(
        &env,
        &attacker,
        &contract,
        "check_in",
        soroban_sdk::vec![&env, id.into_val(&env)],
    );

    assert_rejected!(client.try_check_in(&id));
    // And with no authorization at all.
    refuse_all_auth(&env);
    assert_rejected!(client.try_check_in(&id));
    let _ = owner;
}

#[test]
fn status_is_active_up_to_one_second_before_the_deadline() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    advance(&env, CHECK_IN - ONE);
    assert_eq!(client.get_vault_status(&id), VaultStatus::Active);
    assert_eq!(client.get_vault(&id).effective_status, VaultStatus::Active);
}

#[test]
fn deadline_is_the_first_overdue_instant() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    advance(&env, CHECK_IN);
    assert_eq!(
        client.get_vault_status(&id),
        VaultStatus::GracePeriod,
        "at exactly the deadline the vault is already overdue"
    );
    let deadlines = client.get_deadlines(&id);
    assert_eq!(deadlines.now, deadlines.deadline);
    assert_eq!(deadlines.grace_end, START + CHECK_IN + GRACE);
}

#[test]
fn owner_may_still_check_in_during_the_grace_period() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    advance(&env, CHECK_IN + ONE);
    assert_eq!(client.get_vault_status(&id), VaultStatus::GracePeriod);

    let deadline = client.check_in(&id);
    assert_eq!(client.get_vault_status(&id), VaultStatus::Active);
    assert_eq!(deadline, START + CHECK_IN + ONE + CHECK_IN);
}

#[test]
fn owner_may_check_in_one_second_before_grace_expires() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    advance(&env, CHECK_IN + GRACE - ONE);
    client.check_in(&id);
    assert_eq!(client.get_vault_status(&id), VaultStatus::Active);
}

#[test]
fn check_in_is_refused_at_grace_expiry() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    advance(&env, CHECK_IN + GRACE);
    assert_contract_error!(client.try_check_in(&id), HeirVaultError::GracePeriodExpired);
    assert_eq!(
        client.get_vault_status(&id),
        VaultStatus::GracePeriod,
        "an expired grace period is still the grace-period state until activation"
    );
}

#[test]
fn check_in_is_refused_after_grace_expiry() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    advance(&env, CHECK_IN + GRACE + 10 * ONE);
    assert_contract_error!(client.try_check_in(&id), HeirVaultError::GracePeriodExpired);
}

#[test]
fn check_in_is_refused_after_activation() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    assert_contract_error!(client.try_check_in(&id), HeirVaultError::AlreadyActivated);
}

#[test]
fn check_in_is_refused_after_cancellation() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.cancel_vault(&id);

    assert_contract_error!(client.try_check_in(&id), HeirVaultError::VaultCancelled);
}

#[test]
fn repeated_check_ins_keep_pushing_the_deadline() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    for _ in 0..5 {
        advance(&env, CHECK_IN - ONE);
        client.check_in(&id);
        assert_eq!(client.get_vault_status(&id), VaultStatus::Active);
    }
    assert!(
        client.get_deadlines(&id).deadline > START + 4 * CHECK_IN,
        "a vault checked in on time must never reach its deadline"
    );
}

#[test]
fn deadlines_track_the_current_ledger_time() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    advance(&env, 12345);
    let d = client.get_deadlines(&id);
    assert_eq!(d.now, START + 12345);
    assert_eq!(d.deadline, START + CHECK_IN);
    assert_eq!(d.grace_end, START + CHECK_IN + GRACE);
}

// ---------------------------------------------------------------------------
// Activation
// ---------------------------------------------------------------------------

#[test]
fn activation_is_refused_before_the_deadline() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    advance(&env, CHECK_IN - ONE);
    assert!(!client.get_vault(&id).activation_ready);
    assert_contract_error!(
        client.try_activate_vault(&id),
        HeirVaultError::DeadlineNotReached
    );
    assert_eq!(client.get_vault_status(&id), VaultStatus::Active);
}

#[test]
fn activation_is_refused_at_the_deadline() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    advance(&env, CHECK_IN);
    assert_contract_error!(
        client.try_activate_vault(&id),
        HeirVaultError::GracePeriodActive
    );
}

#[test]
fn activation_is_refused_during_the_grace_period() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    advance(&env, CHECK_IN + GRACE - ONE);
    assert_contract_error!(
        client.try_activate_vault(&id),
        HeirVaultError::GracePeriodActive
    );
    assert!(!client.get_vault(&id).activation_ready);
}

#[test]
fn activation_succeeds_at_grace_expiry() {
    let env = Env::default();
    let (id, contract, asset, _, heir, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    advance(&env, CHECK_IN + GRACE);
    let info = client.get_vault(&id);
    assert!(
        info.activation_ready,
        "grace_end is inclusive for activation"
    );
    assert!(!client.is_claimable(&id));

    let (_, events) = capture!(&env, client.activate_vault(&id));
    assert!(has_event(&env, &events, "vault_activated"));

    let info = client.get_vault(&id);
    assert_eq!(info.vault.status, VaultStatus::Activated);
    assert_eq!(info.vault.activated_at, START + CHECK_IN + GRACE);
    assert_eq!(info.vault.distribution_balance, 100 * UNIT);
    assert_eq!(info.vault.total_claimed, 0);
    assert_eq!(info.vault.claimed_count, 0);
    assert!(client.is_claimable(&id));

    // Nothing moved yet: activation only opens the claim phase.
    assert_eq!(balance(&env, &asset, &contract), 100 * UNIT);
    assert_eq!(balance(&env, &asset, &heir), 0);
}

#[test]
fn activation_is_permissionless() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE);

    // No authorization mocked at all: a keeper must be able to push a due vault.
    refuse_all_auth(&env);
    client.activate_vault(&id);
    assert_eq!(client.get_vault_status(&id), VaultStatus::Activated);
}

#[test]
fn activation_cannot_be_repeated() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    assert_contract_error!(
        client.try_activate_vault(&id),
        HeirVaultError::AlreadyActivated
    );
}

#[test]
fn activation_requires_at_least_one_beneficiary() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    mint(&env, &asset, &owner, 100 * UNIT);
    client.deposit(&id, &asset, &(100 * UNIT));

    advance(&env, CHECK_IN + GRACE);
    assert_contract_error!(
        client.try_activate_vault(&id),
        HeirVaultError::NoBeneficiaries
    );
}

#[test]
fn activation_requires_a_full_one_hundred_percent_allocation() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    client.add_beneficiary(&id, &Address::generate(&env), &9_999u32);
    mint(&env, &asset, &owner, 100 * UNIT);
    client.deposit(&id, &asset, &(100 * UNIT));

    advance(&env, CHECK_IN + GRACE);
    assert_contract_error!(
        client.try_activate_vault(&id),
        HeirVaultError::AllocationIncomplete
    );

    // Filling the last basis point is enough.
    client.add_beneficiary(&id, &Address::generate(&env), &1u32);
    client.activate_vault(&id);
    assert_eq!(client.get_vault_status(&id), VaultStatus::Activated);
}

#[test]
fn activation_requires_a_non_zero_balance() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    client.add_beneficiary(&id, &Address::generate(&env), &10_000u32);

    advance(&env, CHECK_IN + GRACE);
    assert_contract_error!(
        client.try_activate_vault(&id),
        HeirVaultError::InsufficientBalance
    );
    assert!(!client.get_vault(&id).activation_ready);
}

#[test]
fn activation_is_refused_on_a_cancelled_vault() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.cancel_vault(&id);

    advance(&env, CHECK_IN + GRACE);
    assert_contract_error!(
        client.try_activate_vault(&id),
        HeirVaultError::VaultCancelled
    );
}

#[test]
fn guardian_approval_mode_activates_without_waiting_for_the_deadline() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);

    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    let g3 = Address::generate(&env);
    client.add_guardian(&id, &g1);
    client.add_guardian(&id, &g2);
    client.add_guardian(&id, &g3);
    client.set_guardian_threshold(&id, &2u32);

    // Still well inside the check-in period, so the clock condition is unmet.
    advance(&env, ONE);
    assert_contract_error!(client.try_activate_vault(&id), HeirVaultError::NotEligible);

    client.guardian_approve(&id, &g1);
    assert_contract_error!(client.try_activate_vault(&id), HeirVaultError::NotEligible);

    client.guardian_approve(&id, &g3);
    let status = client.get_guardian_status(&id);
    assert_eq!(status.approvals, 2);
    assert_eq!(status.threshold, 2);
    assert!(status.threshold_met);

    client.activate_vault(&id);
    assert_eq!(client.get_vault_status(&id), VaultStatus::Activated);
}

#[test]
fn multi_condition_mode_activates_only_when_both_are_met() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MultiCondition, 100 * UNIT);
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    client.add_guardian(&id, &g1);
    client.add_guardian(&id, &g2);
    client.set_guardian_threshold(&id, &2u32);

    advance(&env, CHECK_IN + GRACE);
    assert_contract_error!(client.try_activate_vault(&id), HeirVaultError::NotEligible);

    client.guardian_approve(&id, &g1);
    assert_contract_error!(client.try_activate_vault(&id), HeirVaultError::NotEligible);

    client.guardian_approve(&id, &g2);
    client.activate_vault(&id);
    assert_eq!(client.get_vault_status(&id), VaultStatus::Activated);
}

#[test]
fn missed_check_in_mode_ignores_guardians_for_activation_eligibility() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.add_guardian(&id, &Address::generate(&env));

    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);
    assert_eq!(client.get_vault_status(&id), VaultStatus::Activated);
}

// ---------------------------------------------------------------------------
// Claims
// ---------------------------------------------------------------------------

#[test]
fn claim_before_activation_is_rejected() {
    let env = Env::default();
    let (id, _, _, _, heir, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    assert_contract_error!(client.try_claim(&id, &heir), HeirVaultError::NotActivated);
}

#[test]
fn claim_during_the_grace_period_is_rejected() {
    let env = Env::default();
    let (id, _, _, _, heir, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + ONE);

    assert_contract_error!(client.try_claim(&id, &heir), HeirVaultError::NotActivated);
}

#[test]
fn claim_transfers_the_whole_allocation_for_a_single_heir() {
    let env = Env::default();
    let (id, contract, asset, _, heir, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    let (paid, events) = capture!(&env, client.claim(&id, &heir));
    assert!(has_event(&env, &events, "inheritance_claimed"));
    assert!(has_event(&env, &events, "vault_completed"));

    assert_eq!(paid, 100 * UNIT);
    assert_eq!(balance(&env, &asset, &heir), 100 * UNIT);
    assert_eq!(
        balance(&env, &asset, &contract),
        0,
        "the vault is fully drained"
    );

    let info = client.get_vault(&id);
    assert_eq!(info.vault.status, VaultStatus::Completed);
    assert_eq!(info.vault.balance, 0);
    assert_eq!(info.vault.total_claimed, 100 * UNIT);
    assert_eq!(info.vault.claimed_count, 1);
}

#[test]
fn claim_distributes_proportionally_across_heirs() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let (contract, asset) = deploy(&env);
    let client = HeirVaultContractClient::new(&env, &contract);
    let owner = Address::generate(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    let c = Address::generate(&env);
    client.add_beneficiary(&id, &a, &5_000u32);
    client.add_beneficiary(&id, &b, &3_000u32);
    client.add_beneficiary(&id, &c, &2_000u32);

    mint(&env, &asset, &owner, 1_000 * UNIT);
    client.deposit(&id, &asset, &(1_000 * UNIT));
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    assert_eq!(client.claim(&id, &a), 500 * UNIT);
    assert_eq!(client.claim(&id, &b), 300 * UNIT);
    assert_eq!(client.claim(&id, &c), 200 * UNIT);

    assert_eq!(balance(&env, &asset, &a), 500 * UNIT);
    assert_eq!(balance(&env, &asset, &b), 300 * UNIT);
    assert_eq!(balance(&env, &asset, &c), 200 * UNIT);
    assert_eq!(balance(&env, &asset, &contract), 0);
    assert_eq!(client.get_vault_status(&id), VaultStatus::Completed);
}

#[test]
fn rounding_dust_goes_to_the_last_heir_to_claim() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let (contract, asset) = deploy(&env);
    let client = HeirVaultContractClient::new(&env, &contract);
    let owner = Address::generate(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    let c = Address::generate(&env);
    // A three-way split that cannot divide evenly: 1/3, 1/3, and the remainder.
    client.add_beneficiary(&id, &a, &3_333u32);
    client.add_beneficiary(&id, &b, &3_333u32);
    client.add_beneficiary(&id, &c, &3_334u32);

    // 100 base units: floor shares are 33, 33 and 33, leaving one unit unallocated.
    mint(&env, &asset, &owner, 100);
    client.deposit(&id, &asset, &100);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    assert_eq!(client.claim(&id, &a), 33);
    assert_eq!(client.claim(&id, &b), 33);
    assert_eq!(client.claim(&id, &c), 34, "the last heir absorbs the dust");

    assert_eq!(balance(&env, &asset, &contract), 0);
    assert_eq!(client.get_vault(&id).vault.total_claimed, 100);
    assert_eq!(client.get_vault(&id).vault.status, VaultStatus::Completed);
}

#[test]
fn claim_order_does_not_change_the_total_distributed() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let (contract, asset) = deploy(&env);
    let client = HeirVaultContractClient::new(&env, &contract);
    let owner = Address::generate(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    client.add_beneficiary(&id, &a, &6_666u32);
    client.add_beneficiary(&id, &b, &3_334u32);

    mint(&env, &asset, &owner, 999);
    client.deposit(&id, &asset, &999);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    // Claim in the opposite order to the registration order.
    let b_paid = client.claim(&id, &b);
    let a_paid = client.claim(&id, &a);

    assert_eq!(b_paid, 333, "a non-final heir gets its floored share");
    assert_eq!(a_paid, 666, "the final heir receives the remaining balance");
    assert_eq!(a_paid + b_paid, 999);
    assert_eq!(balance(&env, &asset, &contract), 0);
}

#[test]
fn double_claim_is_rejected() {
    let env = Env::default();
    let (id, _, _, _, heir, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    client.claim(&id, &heir);
    // The vault is COMPLETED now, which is the precise reason reported.
    assert_contract_error!(client.try_claim(&id, &heir), HeirVaultError::VaultCompleted);
}

#[test]
fn double_claim_is_rejected_while_other_heirs_remain() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let (contract, asset) = deploy(&env);
    let client = HeirVaultContractClient::new(&env, &contract);
    let owner = Address::generate(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    client.add_beneficiary(&id, &a, &5_000u32);
    client.add_beneficiary(&id, &b, &5_000u32);
    mint(&env, &asset, &owner, 100 * UNIT);
    client.deposit(&id, &asset, &(100 * UNIT));
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    client.claim(&id, &a);
    assert_contract_error!(client.try_claim(&id, &a), HeirVaultError::AlreadyClaimed);

    // B is unaffected and still receives its full share.
    assert_eq!(client.claim(&id, &b), 50 * UNIT);
}

#[test]
fn unregistered_addresses_cannot_claim() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    let stranger = Address::generate(&env);
    assert_contract_error!(
        client.try_claim(&id, &stranger),
        HeirVaultError::InvalidBeneficiary
    );
}

#[test]
fn claim_requires_the_beneficiary_to_authenticate() {
    let env = Env::default();
    let (id, contract, _, _, heir, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    // Only a stranger is authorized: the contract must still demand the heir.
    let attacker = Address::generate(&env);
    authorize_only(
        &env,
        &attacker,
        &contract,
        "claim",
        soroban_sdk::vec![&env, id.into_val(&env), heir.to_val()],
    );
    assert_rejected!(client.try_claim(&id, &heir));

    refuse_all_auth(&env);
    assert_rejected!(client.try_claim(&id, &heir));
}

#[test]
fn claim_is_rejected_on_a_cancelled_vault() {
    let env = Env::default();
    let (id, _, _, _, heir, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.cancel_vault(&id);

    assert_contract_error!(client.try_claim(&id, &heir), HeirVaultError::VaultCancelled);
}

#[test]
fn claim_position_is_reported_before_and_after_claiming() {
    let env = Env::default();
    let (id, _, _, _, heir, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    let before = client.get_claim(&id, &heir);
    assert_eq!(before.entitlement, 100 * UNIT);
    assert_eq!(before.claimed_amount, 0);
    assert!(!before.claimed);
    assert_eq!(client.get_undistributed_balance(&id), 100 * UNIT);

    client.claim(&id, &heir);

    let after = client.get_claim(&id, &heir);
    assert_eq!(after.claimed_amount, 100 * UNIT);
    assert!(after.claimed);
    assert_eq!(client.get_undistributed_balance(&id), 0);
}

#[test]
fn undistributed_balance_is_zero_before_activation() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    assert_eq!(client.get_undistributed_balance(&id), 0);
}

// ---------------------------------------------------------------------------
// Cancellation and withdrawal
// ---------------------------------------------------------------------------

#[test]
fn cancel_marks_the_vault_cancelled_without_moving_funds() {
    let env = Env::default();
    let (id, contract, asset, _, _, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    let (_, events) = capture!(&env, client.cancel_vault(&id));
    assert!(has_event(&env, &events, "vault_cancelled"));

    assert_eq!(client.get_vault_status(&id), VaultStatus::Cancelled);
    assert_eq!(client.get_vault(&id).vault.balance, 100 * UNIT);
    assert_eq!(balance(&env, &asset, &contract), 100 * UNIT);
}

#[test]
fn cancel_is_allowed_during_the_grace_period() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + ONE);

    client.cancel_vault(&id);
    assert_eq!(client.get_vault_status(&id), VaultStatus::Cancelled);
}

#[test]
fn cancel_is_allowed_after_grace_expiry_while_still_unactivated() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE + ONE);

    // An authenticated owner is demonstrably alive; ownership only passes on an
    // explicit activation.
    client.cancel_vault(&id);
    assert_eq!(client.get_vault_status(&id), VaultStatus::Cancelled);
}

#[test]
fn cancel_is_rejected_after_activation() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    assert_contract_error!(
        client.try_cancel_vault(&id),
        HeirVaultError::AlreadyActivated
    );
}

#[test]
fn cancel_cannot_be_repeated() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.cancel_vault(&id);

    assert_contract_error!(client.try_cancel_vault(&id), HeirVaultError::VaultCancelled);
}

#[test]
fn cancel_requires_owner_authentication() {
    let env = Env::default();
    let (id, contract, _, _, _, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    let attacker = Address::generate(&env);
    authorize_only(
        &env,
        &attacker,
        &contract,
        "cancel_vault",
        soroban_sdk::vec![&env, id.into_val(&env)],
    );
    assert_rejected!(client.try_cancel_vault(&id));

    refuse_all_auth(&env);
    assert_rejected!(client.try_cancel_vault(&id));
}

#[test]
fn withdraw_returns_funds_to_the_owner_after_cancellation() {
    let env = Env::default();
    let (id, contract, asset, owner, _, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.cancel_vault(&id);
    assert_eq!(balance(&env, &asset, &owner), 0);

    let (_, events) = capture!(&env, client.withdraw(&id, &(100 * UNIT)));
    assert!(has_event(&env, &events, "withdrawal"));

    assert_eq!(balance(&env, &asset, &owner), 100 * UNIT);
    assert_eq!(balance(&env, &asset, &contract), 0);
    assert_eq!(client.get_vault(&id).vault.balance, 0);
}

#[test]
fn withdraw_supports_partial_amounts() {
    let env = Env::default();
    let (id, _, asset, owner, _, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.cancel_vault(&id);

    client.withdraw(&id, &(30 * UNIT));
    assert_eq!(balance(&env, &asset, &owner), 30 * UNIT);
    assert_eq!(client.get_vault(&id).vault.balance, 70 * UNIT);

    client.withdraw(&id, &(70 * UNIT));
    assert_eq!(balance(&env, &asset, &owner), 100 * UNIT);
    assert_eq!(client.get_vault(&id).vault.balance, 0);
}

#[test]
fn withdraw_cannot_exceed_the_balance() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.cancel_vault(&id);

    assert_contract_error!(
        client.try_withdraw(&id, &(100 * UNIT + 1)),
        HeirVaultError::InsufficientBalance
    );
}

#[test]
fn withdraw_rejects_non_positive_amounts() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.cancel_vault(&id);

    assert_contract_error!(client.try_withdraw(&id, &0), HeirVaultError::InvalidAmount);
    assert_contract_error!(client.try_withdraw(&id, &-5), HeirVaultError::InvalidAmount);
}

#[test]
fn withdraw_is_rejected_before_cancellation() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    assert_contract_error!(
        client.try_withdraw(&id, &UNIT),
        HeirVaultError::InvalidState
    );
}

#[test]
fn withdraw_is_rejected_after_activation() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    assert_contract_error!(
        client.try_withdraw(&id, &UNIT),
        HeirVaultError::AlreadyActivated
    );
}

#[test]
fn withdraw_requires_owner_authentication() {
    let env = Env::default();
    let (id, contract, _, _, _, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.cancel_vault(&id);

    let attacker = Address::generate(&env);
    authorize_only(
        &env,
        &attacker,
        &contract,
        "withdraw",
        soroban_sdk::vec![&env, id.into_val(&env), UNIT.into_val(&env)],
    );
    assert_rejected!(client.try_withdraw(&id, &UNIT));

    refuse_all_auth(&env);
    assert_rejected!(client.try_withdraw(&id, &UNIT));
    let _ = contract;
}

#[test]
fn a_cancelled_vault_cannot_be_claimed_from() {
    let env = Env::default();
    let (id, _, asset, owner, heir, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.cancel_vault(&id);
    client.withdraw(&id, &(100 * UNIT));

    assert_contract_error!(client.try_claim(&id, &heir), HeirVaultError::VaultCancelled);
    assert_eq!(balance(&env, &asset, &owner), 100 * UNIT);
    assert_eq!(balance(&env, &asset, &heir), 0);
}

// ---------------------------------------------------------------------------
// Guardians
// ---------------------------------------------------------------------------

#[test]
fn first_guardian_gets_a_single_approval_threshold() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);
    let g1 = Address::generate(&env);

    let (_, events) = capture!(&env, client.add_guardian(&id, &g1));
    assert!(has_event(&env, &events, "guardian_added"));

    let status = client.get_guardian_status(&id);
    assert_eq!(status.guardian_count, 1);
    assert_eq!(status.threshold, 1);
    assert_eq!(status.approvals, 0);
    assert!(!status.threshold_met);
}

#[test]
fn guardians_can_be_added_up_to_the_cap() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);

    for _ in 0..crate::MAX_GUARDIANS {
        client.add_guardian(&id, &Address::generate(&env));
    }
    assert_contract_error!(
        client.try_add_guardian(&id, &Address::generate(&env)),
        HeirVaultError::TooManyGuardians
    );
}

#[test]
fn duplicate_guardians_are_rejected() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);
    let g1 = Address::generate(&env);

    client.add_guardian(&id, &g1);
    assert_contract_error!(
        client.try_add_guardian(&id, &g1),
        HeirVaultError::DuplicateGuardian
    );
}

#[test]
fn guardians_may_not_be_the_owner_or_the_contract() {
    let env = Env::default();
    let (contract, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::GuardianApproval,
        &CHECK_IN,
        &GRACE,
    );

    assert_contract_error!(
        client.try_add_guardian(&id, &owner),
        HeirVaultError::InvalidAddress
    );
    assert_contract_error!(
        client.try_add_guardian(&id, &contract),
        HeirVaultError::InvalidAddress
    );
}

#[test]
fn removing_a_guardian_keeps_a_retained_slot() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    client.add_guardian(&id, &g1);
    client.add_guardian(&id, &g2);

    let (_, events) = capture!(&env, client.remove_guardian(&id, &g2));
    assert!(has_event(&env, &events, "guardian_removed"));

    let status = client.get_guardian_status(&id);
    assert_eq!(status.guardian_count, 1);
    assert_eq!(
        client.get_guardians(&id, &0, &10).meta.total,
        2,
        "the slot is retained"
    );
}

#[test]
fn removing_a_guardian_is_rejected_when_it_breaks_the_threshold() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    client.add_guardian(&id, &g1);
    client.add_guardian(&id, &g2);
    client.set_guardian_threshold(&id, &2u32);

    assert_contract_error!(
        client.try_remove_guardian(&id, &g2),
        HeirVaultError::InvalidThreshold
    );

    // Lowering the bar first makes the removal legal.
    client.set_guardian_threshold(&id, &1u32);
    client.remove_guardian(&id, &g2);
    assert_eq!(client.get_guardian_status(&id).guardian_count, 1);
}

#[test]
fn the_last_guardian_of_a_guardian_gated_vault_cannot_be_removed() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);
    let g1 = Address::generate(&env);
    client.add_guardian(&id, &g1);

    assert_contract_error!(
        client.try_remove_guardian(&id, &g1),
        HeirVaultError::InvalidThreshold
    );
}

#[test]
fn the_last_guardian_of_a_missed_check_in_vault_may_be_removed() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    let g1 = Address::generate(&env);
    client.add_guardian(&id, &g1);

    client.remove_guardian(&id, &g1);
    let status = client.get_guardian_status(&id);
    assert_eq!(status.guardian_count, 0);
    assert_eq!(
        status.threshold, 0,
        "a guardian-free vault needs no threshold"
    );
}

#[test]
fn threshold_must_be_positive_and_within_the_guardian_count() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    let g3 = Address::generate(&env);
    client.add_guardian(&id, &g1);
    client.add_guardian(&id, &g2);
    client.add_guardian(&id, &g3);

    assert_contract_error!(
        client.try_set_guardian_threshold(&id, &0u32),
        HeirVaultError::InvalidThreshold
    );
    assert_contract_error!(
        client.try_set_guardian_threshold(&id, &4u32),
        HeirVaultError::InvalidThreshold
    );

    let (_, events) = capture!(&env, client.set_guardian_threshold(&id, &3u32));
    assert!(has_event(&env, &events, "guardian_threshold_updated"));
    assert_eq!(client.get_guardian_status(&id).threshold, 3);
}

#[test]
fn a_guardian_can_approve_once_per_attempt() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);
    let g1 = Address::generate(&env);
    client.add_guardian(&id, &g1);

    let (_, events) = capture!(&env, client.guardian_approve(&id, &g1));
    assert!(has_event(&env, &events, "guardian_approved"));
    assert_eq!(client.get_guardian_status(&id).approvals, 1);

    assert_contract_error!(
        client.try_guardian_approve(&id, &g1),
        HeirVaultError::AlreadyApproved
    );
    assert_eq!(
        client.get_guardian_status(&id).approvals,
        1,
        "the count must not inflate"
    );
}

#[test]
fn unregistered_guardians_cannot_approve() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);
    client.add_guardian(&id, &Address::generate(&env));

    assert_contract_error!(
        client.try_guardian_approve(&id, &Address::generate(&env)),
        HeirVaultError::InvalidGuardian
    );
}

#[test]
fn a_removed_guardian_cannot_approve() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    client.add_guardian(&id, &g1);
    client.add_guardian(&id, &g2);
    client.remove_guardian(&id, &g2);

    assert_contract_error!(
        client.try_guardian_approve(&id, &g2),
        HeirVaultError::InvalidGuardian
    );
}

#[test]
fn guardian_approval_requires_the_guardian_to_authenticate() {
    let env = Env::default();
    let (id, contract, _, _, _, client) =
        funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);
    let g1 = Address::generate(&env);
    client.add_guardian(&id, &g1);

    let attacker = Address::generate(&env);
    authorize_only(
        &env,
        &attacker,
        &contract,
        "guardian_approve",
        soroban_sdk::vec![&env, id.into_val(&env), g1.to_val()],
    );
    assert_rejected!(client.try_guardian_approve(&id, &g1));

    refuse_all_auth(&env);
    assert_rejected!(client.try_guardian_approve(&id, &g1));
    assert_eq!(client.get_guardian_status(&id).approvals, 0);
}

#[test]
fn approvals_are_rejected_when_the_mode_does_not_use_guardians() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    let g1 = Address::generate(&env);
    client.add_guardian(&id, &g1);

    assert_contract_error!(
        client.try_guardian_approve(&id, &g1),
        HeirVaultError::ApprovalNotEnabled
    );
}

#[test]
fn a_check_in_clears_every_guardian_approval() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    client.add_guardian(&id, &g1);
    client.add_guardian(&id, &g2);
    client.set_guardian_threshold(&id, &2u32);

    client.guardian_approve(&id, &g1);
    client.guardian_approve(&id, &g2);
    assert!(client.get_guardian_status(&id).threshold_met);

    // The owner proves they are alive, invalidating the attempt.
    client.check_in(&id);

    let status = client.get_guardian_status(&id);
    assert_eq!(status.approvals, 0);
    assert!(!status.threshold_met);

    // Both guardians may vote again on the new attempt.
    client.guardian_approve(&id, &g1);
    client.guardian_approve(&id, &g2);
    assert_eq!(client.get_guardian_status(&id).approvals, 2);
}

#[test]
fn a_check_in_rescues_a_guardian_gated_vault() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);
    let g1 = Address::generate(&env);
    let g2 = Address::generate(&env);
    client.add_guardian(&id, &g1);
    client.add_guardian(&id, &g2);
    client.set_guardian_threshold(&id, &2u32);

    client.guardian_approve(&id, &g1);
    client.guardian_approve(&id, &g2);
    client.check_in(&id);

    advance(&env, ONE);
    assert_contract_error!(client.try_activate_vault(&id), HeirVaultError::NotEligible);
}

#[test]
fn guardian_configuration_is_frozen_after_activation() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.add_guardian(&id, &Address::generate(&env));
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    assert_contract_error!(
        client.try_add_guardian(&id, &Address::generate(&env)),
        HeirVaultError::AlreadyActivated
    );
    assert_contract_error!(
        client.try_set_guardian_threshold(&id, &1u32),
        HeirVaultError::AlreadyActivated
    );
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

#[test]
fn vaults_are_paged_per_owner() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let other = Address::generate(&env);

    for _ in 0..7 {
        client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE,
        );
    }
    client.create_vault(
        &other,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    let first = client.get_vaults_by_owner(&owner, &0, &3);
    assert_eq!(first.items.len(), 3);
    assert_eq!(first.meta.total, 7, "only this owner's vaults are counted");
    assert_eq!(first.meta.offset, 0);
    assert_eq!(first.meta.limit, 3);
    assert!(first.meta.has_more);
    assert_eq!(first.meta.next_offset, Some(3));

    let second = client.get_vaults_by_owner(&owner, &3, &3);
    assert_eq!(second.items.len(), 3);
    assert!(second.meta.has_more);
    assert_eq!(second.meta.next_offset, Some(6));

    let last = client.get_vaults_by_owner(&owner, &6, &3);
    assert_eq!(last.items.len(), 1);
    assert!(!last.meta.has_more);
    assert_eq!(last.meta.next_offset, None);
    assert_eq!(last.items.get(0).unwrap().owner, owner);

    // Offsets are stable: pages never overlap or skip.
    let mut seen = StdVec::new();
    for offset in [0u32, 3, 6] {
        for summary in client.get_vaults_by_owner(&owner, &offset, &3).items.iter() {
            seen.push(summary.id);
        }
    }
    seen.sort_unstable();
    assert_eq!(seen, std::vec![1, 2, 3, 4, 5, 6, 7]);
}

#[test]
fn paging_past_the_end_returns_an_empty_page() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    let page = client.get_vaults_by_owner(&owner, &10, &5);
    assert_eq!(page.items.len(), 0);
    assert_eq!(page.meta.total, 1);
    assert!(!page.meta.has_more);
    assert_eq!(page.meta.next_offset, None);
}

#[test]
fn a_zero_page_limit_is_rejected() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    assert_contract_error!(
        client.try_get_vaults_by_owner(&owner, &0, &0),
        HeirVaultError::InvalidPageLimit
    );
    assert_contract_error!(
        client.try_get_beneficiaries(&id, &0, &0),
        HeirVaultError::InvalidPageLimit
    );
    assert_contract_error!(
        client.try_get_guardians(&id, &0, &0),
        HeirVaultError::InvalidPageLimit
    );
    assert_contract_error!(
        client.try_get_claims(&id, &0, &0),
        HeirVaultError::InvalidPageLimit
    );
}

#[test]
fn an_oversized_page_limit_is_clamped() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    for _ in 0..5 {
        client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE,
        );
    }

    let page = client.get_vaults_by_owner(&owner, &0, &u32::MAX);
    assert_eq!(
        page.items.len(),
        5,
        "a bounded, simulatable result, not an error"
    );
    assert_eq!(page.meta.limit, crate::MAX_PAGE_LIMIT);
}

#[test]
fn beneficiaries_guardians_and_claims_page_independently() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let (contract, asset) = deploy(&env);
    let client = HeirVaultContractClient::new(&env, &contract);
    let owner = Address::generate(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    for _ in 0..5 {
        client.add_beneficiary(&id, &Address::generate(&env), &2_000u32);
    }
    for _ in 0..3 {
        client.add_guardian(&id, &Address::generate(&env));
    }

    let b_page = client.get_beneficiaries(&id, &1, &2);
    assert_eq!(b_page.items.len(), 2);
    assert_eq!(b_page.meta.total, 5);
    assert_eq!(b_page.meta.next_offset, Some(3));

    let g_page = client.get_guardians(&id, &2, &2);
    assert_eq!(g_page.items.len(), 1);
    assert_eq!(g_page.meta.total, 3);
    assert!(!g_page.meta.has_more);

    mint(&env, &asset, &owner, 100 * UNIT);
    client.deposit(&id, &asset, &(100 * UNIT));
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    let c_page = client.get_claims(&id, &0, &2);
    assert_eq!(c_page.items.len(), 2);
    assert_eq!(c_page.meta.total, 5);
    assert_eq!(c_page.items.get(0).unwrap().entitlement, 20 * UNIT);
    assert!(!c_page.items.get(0).unwrap().claimed);
    assert!(c_page.meta.has_more);
}

// ---------------------------------------------------------------------------
// Authorization matrix
// ---------------------------------------------------------------------------

/// Every privileged entry point must refuse a caller that is not the address the
/// contract requires, even when that caller holds a valid authorization for the
/// call.
#[test]
fn privileged_entry_points_reject_the_wrong_authorizer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let (contract, asset) = deploy(&env);
    let client = HeirVaultContractClient::new(&env, &contract);
    let owner = Address::generate(&env);
    let heir = Address::generate(&env);
    let guardian = Address::generate(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::GuardianApproval,
        &CHECK_IN,
        &GRACE,
    );
    client.add_beneficiary(&id, &heir, &10_000u32);
    client.add_guardian(&id, &guardian);
    mint(&env, &asset, &owner, 100 * UNIT);
    client.deposit(&id, &asset, &(100 * UNIT));

    let attacker = Address::generate(&env);
    let args1 = soroban_sdk::vec![&env, id.into_val(&env)];
    let args1_addr = soroban_sdk::vec![&env, id.into_val(&env), heir.to_val()];
    let args1_u32 = soroban_sdk::vec![&env, id.into_val(&env), 1u32.into_val(&env)];

    // Owner-only entry points.
    authorize_only(
        &env,
        &attacker,
        &contract,
        "add_beneficiary",
        args1_addr.clone(),
    );
    assert_rejected!(client.try_add_beneficiary(&id, &Address::generate(&env), &1u32));

    authorize_only(
        &env,
        &attacker,
        &contract,
        "remove_beneficiary",
        args1_addr.clone(),
    );
    assert_rejected!(client.try_remove_beneficiary(&id, &heir));

    authorize_only(
        &env,
        &attacker,
        &contract,
        "update_beneficiary",
        args1_addr.clone(),
    );
    assert_rejected!(client.try_update_beneficiary(&id, &heir, &5_000u32));

    authorize_only(
        &env,
        &attacker,
        &contract,
        "add_guardian",
        args1_addr.clone(),
    );
    assert_rejected!(client.try_add_guardian(&id, &Address::generate(&env)));

    authorize_only(
        &env,
        &attacker,
        &contract,
        "remove_guardian",
        args1_addr.clone(),
    );
    assert_rejected!(client.try_remove_guardian(&id, &guardian));

    authorize_only(
        &env,
        &attacker,
        &contract,
        "set_guardian_threshold",
        args1_u32.clone(),
    );
    assert_rejected!(client.try_set_guardian_threshold(&id, &1u32));

    authorize_only(&env, &attacker, &contract, "check_in", args1.clone());
    assert_rejected!(client.try_check_in(&id));

    authorize_only(&env, &attacker, &contract, "cancel_vault", args1.clone());
    assert_rejected!(client.try_cancel_vault(&id));

    authorize_only(
        &env,
        &attacker,
        &contract,
        "deposit",
        soroban_sdk::vec![&env, id.into_val(&env), asset.to_val(), UNIT.into_val(&env)],
    );
    assert_rejected!(client.try_deposit(&id, &asset, &UNIT));

    // `claim` and `withdraw` require a different (non-owner) authorizer and are
    // covered by their own tests, where the correct authorization can be set up
    // without disturbing this one's restricted auth state.
}

#[test]
fn every_privileged_call_fails_without_any_authorization() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let (contract, asset) = deploy(&env);
    let client = HeirVaultContractClient::new(&env, &contract);
    let owner = Address::generate(&env);
    let heir = Address::generate(&env);
    let guardian = Address::generate(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::GuardianApproval,
        &CHECK_IN,
        &GRACE,
    );
    client.add_beneficiary(&id, &heir, &10_000u32);
    client.add_guardian(&id, &guardian);
    mint(&env, &asset, &owner, 100 * UNIT);
    client.deposit(&id, &asset, &(100 * UNIT));

    refuse_all_auth(&env);

    assert_rejected!(client.try_create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE
    ));
    assert_rejected!(client.try_deposit(&id, &asset, &UNIT));
    assert_rejected!(client.try_add_beneficiary(&id, &Address::generate(&env), &1u32));
    assert_rejected!(client.try_remove_beneficiary(&id, &heir));
    assert_rejected!(client.try_update_beneficiary(&id, &heir, &9_999u32));
    assert_rejected!(client.try_add_guardian(&id, &Address::generate(&env)));
    assert_rejected!(client.try_remove_guardian(&id, &guardian));
    assert_rejected!(client.try_set_guardian_threshold(&id, &1u32));
    assert_rejected!(client.try_guardian_approve(&id, &guardian));
    assert_rejected!(client.try_check_in(&id));
    assert_rejected!(client.try_cancel_vault(&id));
    assert_rejected!(client.try_withdraw(&id, &UNIT));
    assert_rejected!(client.try_claim(&id, &heir));
}

// ---------------------------------------------------------------------------
// Invariants
// ---------------------------------------------------------------------------

/// Fishing for an allocation state above 100% across many random sequences.
#[test]
fn invariant_total_allocation_never_exceeds_one_hundred_percent() {
    let env = Env::default();
    let (_, asset, owner, client) = world(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };

    for _ in 0..300 {
        let roll = next() % 100;
        let heir = Address::generate(&env);
        if roll < 55 {
            let bps = (next() % 4_000 + 1) as u32;
            let _ = client.try_add_beneficiary(&id, &heir, &bps);
        } else if roll < 75 {
            let page = client.get_beneficiaries(&id, &0, &crate::MAX_PAGE_LIMIT);
            if !page.items.is_empty() {
                let victim = page
                    .items
                    .get((next() % page.items.len() as u64) as u32)
                    .unwrap();
                if victim.active {
                    let _ = client.try_remove_beneficiary(&id, &victim.address);
                }
            }
        } else {
            let page = client.get_beneficiaries(&id, &0, &crate::MAX_PAGE_LIMIT);
            if !page.items.is_empty() {
                let victim = page
                    .items
                    .get((next() % page.items.len() as u64) as u32)
                    .unwrap();
                let bps = (next() % 4_000 + 1) as u32;
                let _ = client.try_update_beneficiary(&id, &victim.address, &bps);
            }
        }
        let total = client.get_total_allocation(&id);
        assert!(
            total <= 10_000,
            "allocation invariant broken: {total} basis points"
        );
    }
}

#[test]
fn invariant_claimed_never_exceeds_deposited() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let (contract, asset) = deploy(&env);
    let client = HeirVaultContractClient::new(&env, &contract);
    let owner = Address::generate(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    // 4 heirs at 2500 bps each, summing to exactly 100%.
    let heirs: StdVec<Address> = (0..4).map(|_| Address::generate(&env)).collect();
    for heir in heirs.iter() {
        client.add_beneficiary(&id, heir, &2_500u32);
    }
    mint(&env, &asset, &owner, 1_000 * UNIT);
    client.deposit(&id, &asset, &(1_000 * UNIT));
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    let mut distributed = 0i128;
    for heir in heirs.iter() {
        distributed += client.claim(&id, heir);
        assert!(
            distributed <= 1_000 * UNIT,
            "distributed more than was deposited"
        );
    }
    assert_eq!(distributed, 1_000 * UNIT);
    assert_eq!(balance(&env, &asset, &contract), 0);
    assert_eq!(client.get_vault(&id).vault.balance, 0);
}

#[test]
fn invariant_an_activated_vault_never_returns_to_active() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);

    for _ in 0..3 {
        advance(&env, CHECK_IN);
        assert_eq!(client.get_vault_status(&id), VaultStatus::Activated);
    }
    assert_contract_error!(client.try_check_in(&id), HeirVaultError::AlreadyActivated);
    assert_contract_error!(
        client.try_activate_vault(&id),
        HeirVaultError::AlreadyActivated
    );
    assert_contract_error!(
        client.try_cancel_vault(&id),
        HeirVaultError::AlreadyActivated
    );
}

#[test]
fn invariant_a_cancelled_vault_never_becomes_active() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    client.cancel_vault(&id);

    for _ in 0..5 {
        advance(&env, CHECK_IN);
        assert_eq!(client.get_vault_status(&id), VaultStatus::Cancelled);
    }
    assert_contract_error!(client.try_check_in(&id), HeirVaultError::VaultCancelled);
    assert_contract_error!(
        client.try_activate_vault(&id),
        HeirVaultError::VaultCancelled
    );
}

#[test]
fn invariant_a_completed_vault_is_fully_settled() {
    let env = Env::default();
    let (id, contract, asset, _, heir, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);
    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&id);
    client.claim(&id, &heir);

    let info = client.get_vault(&id);
    assert_eq!(info.vault.status, VaultStatus::Completed);
    assert_eq!(info.vault.balance, 0);
    assert_eq!(info.vault.total_claimed, info.vault.distribution_balance);
    assert_eq!(balance(&env, &asset, &contract), 0);
    assert_eq!(balance(&env, &asset, &heir), 100 * UNIT);

    // Nothing can restart the vault.
    assert_contract_error!(client.try_claim(&id, &heir), HeirVaultError::VaultCompleted);
    assert_contract_error!(client.try_check_in(&id), HeirVaultError::VaultCompleted);
    assert_contract_error!(
        client.try_activate_vault(&id),
        HeirVaultError::VaultCompleted
    );
    assert_contract_error!(client.try_cancel_vault(&id), HeirVaultError::VaultCompleted);
    assert_contract_error!(client.try_withdraw(&id, &1), HeirVaultError::VaultCompleted);
}

#[test]
fn invariant_guardian_approvals_never_exceed_the_guardian_count() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);
    let guardians: StdVec<Address> = (0..3).map(|_| Address::generate(&env)).collect();
    for guardian in guardians.iter() {
        client.add_guardian(&id, guardian);
    }
    client.set_guardian_threshold(&id, &3u32);

    for _ in 0..3 {
        for guardian in guardians.iter() {
            let _ = client.try_guardian_approve(&id, guardian);
            let status = client.get_guardian_status(&id);
            assert!(
                status.approvals <= status.guardian_count,
                "approvals must never exceed distinct guardians"
            );
        }
    }
    assert_eq!(client.get_guardian_status(&id).approvals, 3);
}

#[test]
fn internal_balance_always_matches_the_tokens_held() {
    let env = Env::default();
    let (id, contract, asset, owner, _, client) =
        funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    mint(&env, &asset, &owner, 50 * UNIT);
    client.deposit(&id, &asset, &(50 * UNIT));
    assert_eq!(
        balance(&env, &asset, &contract),
        client.get_vault(&id).vault.balance
    );

    client.cancel_vault(&id);
    client.withdraw(&id, &(20 * UNIT));
    assert_eq!(
        balance(&env, &asset, &contract),
        client.get_vault(&id).vault.balance
    );
}

// ---------------------------------------------------------------------------
// Events and metadata
// ---------------------------------------------------------------------------

#[test]
fn a_full_lifecycle_publishes_the_documented_events() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let (contract, asset) = deploy(&env);
    let client = HeirVaultContractClient::new(&env, &contract);
    let owner = Address::generate(&env);
    let g1 = Address::generate(&env);
    let first = Address::generate(&env);
    let second = Address::generate(&env);

    // Every name the contract is documented to publish, collected across a whole
    // vault lifetime.
    let mut names: StdVec<Symbol> = StdVec::new();
    macro_rules! step {
        ($call:expr) => {{
            $call;
            names.extend(drain_events(&env));
        }};
    }

    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    names.extend(drain_events(&env));

    step!(client.add_beneficiary(&id, &first, &10_000u32));
    step!(client.update_beneficiary(&id, &first, &6_000u32));
    step!(client.add_beneficiary(&id, &second, &4_000u32));
    step!(client.remove_beneficiary(&id, &second));
    step!(client.add_beneficiary(&id, &second, &4_000u32));
    step!(client.add_guardian(&id, &g1));
    step!(client.set_guardian_threshold(&id, &1u32));
    step!(client.check_in(&id));
    step!(client.remove_guardian(&id, &g1));

    mint(&env, &asset, &owner, 100 * UNIT);
    step!(client.deposit(&id, &asset, &(100 * UNIT)));

    advance(&env, CHECK_IN + GRACE);
    step!(client.activate_vault(&id));
    step!(client.claim(&id, &first));
    step!(client.claim(&id, &second));

    for name in [
        "vault_created",
        "deposit_made",
        "beneficiary_added",
        "beneficiary_updated",
        "beneficiary_removed",
        "guardian_added",
        "guardian_threshold_updated",
        "guardian_removed",
        "check_in_completed",
        "vault_activated",
        "inheritance_claimed",
        "vault_completed",
    ] {
        assert!(has_event(&env, &names, name), "missing event: {name}");
    }

    // The cancelled/withdrawal pair is covered by its own test; here we simply
    // assert the vault reached a fully settled state.
    let info = client.get_vault(&id);
    assert_eq!(info.vault.status, VaultStatus::Completed);
    assert_eq!(info.vault.total_claimed, info.vault.distribution_balance);
}

#[test]
fn cancellation_and_withdrawal_events_are_published() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::MissedCheckIn, 100 * UNIT);

    let mut names: StdVec<Symbol> = StdVec::new();
    macro_rules! step {
        ($call:expr) => {{
            $call;
            names.extend(drain_events(&env));
        }};
    }

    step!(client.cancel_vault(&id));
    step!(client.withdraw(&id, &UNIT));

    assert!(has_event(&env, &names, "vault_cancelled"));
    assert!(has_event(&env, &names, "withdrawal"));
}

#[test]
fn guardian_approval_event_is_published() {
    let env = Env::default();
    let (id, _, _, _, _, client) = funded_vault(&env, ActivationMode::GuardianApproval, 100 * UNIT);
    let g1 = Address::generate(&env);
    client.add_guardian(&id, &g1);
    let (_, events) = capture!(&env, client.guardian_approve(&id, &g1));

    assert!(has_event(&env, &events, "guardian_approved"));
}

#[test]
fn schema_version_is_reported() {
    let env = Env::default();
    let (_, _, _, client) = world(&env);
    assert_eq!(client.schema_version(), crate::SCHEMA_VERSION);
}

#[test]
fn multiple_vaults_for_one_owner_behave_independently() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let (contract, asset) = deploy(&env);
    let client = HeirVaultContractClient::new(&env, &contract);
    let owner = Address::generate(&env);

    let heir = Address::generate(&env);
    let a = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    let b = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    for id in [a, b] {
        client.add_beneficiary(&id, &heir, &10_000u32);
    }
    mint(&env, &asset, &owner, 200 * UNIT);
    client.deposit(&a, &asset, &(100 * UNIT));
    client.deposit(&b, &asset, &(80 * UNIT));

    // Cancelling one vault must not disturb the other.
    client.cancel_vault(&a);
    assert_eq!(client.get_vault_status(&a), VaultStatus::Cancelled);
    assert_eq!(client.get_vault_status(&b), VaultStatus::Active);
    assert_eq!(client.get_vault(&b).vault.balance, 80 * UNIT);

    advance(&env, CHECK_IN + GRACE);
    client.activate_vault(&b);
    assert_eq!(client.claim(&b, &heir), 80 * UNIT);
    assert_eq!(
        balance(&env, &asset, &contract),
        100 * UNIT,
        "vault A's funds are untouched"
    );
}
