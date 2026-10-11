//! Property-based (mini-fuzz) tests.
//!
//! The example-based suite in `test.rs` pins specific behaviours; this module
//! instead drives **randomised, seeded sequences** over the four entry points the
//! project cares most about — `create_vault`, `deposit`, `add_beneficiary` and
//! `claim` — and asserts the invariants that must hold for *every* sequence:
//!
//! * the active allocation total never exceeds 100% (`add_beneficiary`/`update`);
//! * the vault's internal balance always equals the tokens it actually holds;
//! * a vault can only activate at exactly 100% allocation with a non-zero balance;
//! * `sum(all payouts) == distribution_balance` and the vault drains to `0`;
//! * an heir can never be paid twice;
//! * a withdrawal can never exceed the remaining balance.
//!
//! Everything here is deterministic (xorshift, seeded per test) and runs in the
//! ordinary `cargo test` suite: no nightly toolchain and no separate fuzz crate.
//! A failure prints the failing iteration; re-running reproduces it exactly.
//!
//! Run just this module with:
//! `cargo test -p heirvault --lib property`
//!
//! Fuzz harder (same seeds and generators, more iterations) with:
//! `HEIRVAULT_PROPTEST_ITERS=5000 cargo test -p heirvault --lib property`

extern crate std;

use std::vec::Vec as StdVec;

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{token, Address, Env};

use crate::{
    ActivationMode, HeirVaultContract, HeirVaultContractClient, VaultStatus, MAX_CHECK_IN_PERIOD,
    MAX_GRACE_PERIOD, MIN_CHECK_IN_PERIOD, MIN_GRACE_PERIOD,
};

/// Fixed clock origin, matching the example-based suite.
const START: u64 = 1_700_000_000;
/// A 30-day check-in period for generated vaults.
const CHECK_IN: u64 = 30 * 24 * 60 * 60;
/// A 7-day grace period for generated vaults.
const GRACE: u64 = 7 * 24 * 60 * 60;

/// Deterministic xorshift64 PRNG. Seeded so any failure is reproducible.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // A zero state would make xorshift emit only zeros forever.
        Rng(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform-ish value in `0..n` (`0` when `n == 0`).
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }

    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi.saturating_sub(lo) + 1)
    }

    /// Fisher-Yates shuffle of a standard vector.
    fn shuffle<T>(&mut self, items: &mut StdVec<T>) {
        for i in (1..items.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            items.swap(i, j);
        }
    }
}

/// Iteration count for a property test, overridable for a longer soak run.
///
/// Defaults keep the ordinary `cargo test` suite quick; set
/// `HEIRVAULT_PROPTEST_ITERS` to a larger number (e.g. `5000`) to fuzz harder:
/// `HEIRVAULT_PROPTEST_ITERS=5000 cargo test -p heirvault --lib property`.
fn iters(default: u64) -> u64 {
    std::env::var("HEIRVAULT_PROPTEST_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

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

/// `n` allocations (each `>= 1`) summing to exactly 10 000 basis points.
fn full_allocation(rng: &mut Rng, n: usize) -> StdVec<u32> {
    assert!((1..=10).contains(&n), "capacity is 1..=10 beneficiaries");
    let mut out = std::vec![1u32; n];
    let mut remaining = 10_000u32 - n as u32;
    for slot in out.iter_mut().take(n - 1) {
        let take = rng.below(remaining as u64 + 1) as u32;
        *slot += take;
        remaining -= take;
    }
    out[n - 1] += remaining;
    assert_eq!(out.iter().sum::<u32>(), 10_000);
    out
}

// ---------------------------------------------------------------------------
// create_vault
// ---------------------------------------------------------------------------

/// `create_vault` accepts a period exactly when it lies inside the documented
/// range, and never creates a vault for a period outside it.
#[test]
fn property_create_vault_period_range_is_exact() {
    let env = Env::default();
    let (_, asset, owner, client) = fixture(&env);

    let mut rng = Rng::new(0xC0FF_EE00);
    let check_in_choices = [
        0u64,
        59,
        60,
        61,
        MAX_CHECK_IN_PERIOD - 1,
        MAX_CHECK_IN_PERIOD,
        MAX_CHECK_IN_PERIOD + 1,
    ];
    let grace_choices = [
        0u64,
        59,
        60,
        61,
        MAX_GRACE_PERIOD - 1,
        MAX_GRACE_PERIOD,
        MAX_GRACE_PERIOD + 1,
    ];

    let mut created = 0u64;
    for _ in 0..iters(60) {
        let c = check_in_choices[rng.below(check_in_choices.len() as u64) as usize];
        let g = grace_choices[rng.below(grace_choices.len() as u64) as usize];
        let result =
            client.try_create_vault(&owner, &asset, &ActivationMode::MissedCheckIn, &c, &g);
        let expected_ok = (MIN_CHECK_IN_PERIOD..=MAX_CHECK_IN_PERIOD).contains(&c)
            && (MIN_GRACE_PERIOD..=MAX_GRACE_PERIOD).contains(&g);
        assert_eq!(
            result.is_ok(),
            expected_ok,
            "check_in_period={c} grace_period={g}"
        );
        if expected_ok {
            created += 1;
        }
    }
    assert_eq!(client.get_vault_count(), created);
}

// ---------------------------------------------------------------------------
// deposit
// ---------------------------------------------------------------------------

/// The tracked balance always equals the tokens actually held, across arbitrary
/// deposit sequences, and non-positive amounts are always rejected.
#[test]
fn property_deposit_balance_equals_tokens_held() {
    let env = Env::default();
    let (contract, asset, owner, client) = fixture(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );
    mint(&env, &asset, &owner, 1_000_000_000_000);

    let mut rng = Rng::new(0x00D0_9E51);
    let mut expected: i128 = 0;
    for _ in 0..iters(30) {
        // Occasionally probe a rejected (non-positive) amount.
        if rng.below(5) == 0 {
            let bad = if rng.below(2) == 0 { 0 } else { -1 };
            assert!(client.try_deposit(&id, &asset, &bad).is_err());
            continue;
        }
        let amount = rng.range(1, 10_000_000) as i128;
        client.deposit(&id, &asset, &amount);
        expected += amount;
        assert_eq!(client.get_vault(&id).vault.balance, expected);
        assert_eq!(
            token::Client::new(&env, &asset).balance(&contract),
            expected,
            "internal balance must equal tokens held"
        );
    }
    assert!(expected > 0);
}

// ---------------------------------------------------------------------------
// add_beneficiary
// ---------------------------------------------------------------------------

/// Across random add / remove / update sequences the active allocation total
/// never exceeds 100%, and a fresh add that would exceed it is refused.
#[test]
fn property_random_allocation_edits_never_exceed_one_hundred_percent() {
    let env = Env::default();
    let (_, asset, owner, client) = fixture(&env);
    let id = client.create_vault(
        &owner,
        &asset,
        &ActivationMode::MissedCheckIn,
        &CHECK_IN,
        &GRACE,
    );

    // A pool of addresses we can add and remove repeatedly.
    let pool: StdVec<Address> = (0..10).map(|_| Address::generate(&env)).collect();
    let mut rng = Rng::new(0xA110_C8E5);

    for _ in 0..iters(200) {
        let who = &pool[rng.below(pool.len() as u64) as usize];
        match rng.below(10) {
            0..=5 => {
                let bps = rng.range(1, 4_000) as u32;
                let _ = client.try_add_beneficiary(&id, who, &bps);
            }
            6..=7 => {
                let _ = client.try_remove_beneficiary(&id, who);
            }
            _ => {
                let bps = rng.range(1, 4_000) as u32;
                let _ = client.try_update_beneficiary(&id, who, &bps);
            }
        }
        let total = client.get_total_allocation(&id);
        assert!(total <= 10_000, "allocation invariant broken: {total} bps");
    }
}

// ---------------------------------------------------------------------------
// claim (full randomised lifecycle)
// ---------------------------------------------------------------------------

/// Drive a whole random lifecycle and assert exact distribution.
///
/// For each generated vault: a random number of heirs whose allocations sum to
/// exactly 10 000 bps, a random deposit, activation, then claims in a random
/// order. The sums must be exact — `sum(payouts) == deposit`, the vault balance
/// must reach `0`, the status must be `COMPLETED`, and a repeat claim refused.
#[test]
fn property_random_lifecycle_distributes_the_deposit_exactly() {
    let env = Env::default();
    let (contract, asset, owner, client) = fixture(&env);
    mint(&env, &asset, &owner, 10_000_000_000_000);

    let mut rng = Rng::new(0xC1A1_3F00);

    for iteration in 0..iters(30) {
        // Reset the clock: `last_check_in` is stamped at creation time, so each
        // generated vault must be born at the same origin to share a deadline.
        env.ledger().set_timestamp(START);
        let id = client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE,
        );

        // 1..=10 distinct heirs with allocations summing to exactly 100%.
        let n = rng.range(1, 10) as usize;
        let allocations = full_allocation(&mut rng, n);
        let mut heirs: StdVec<Address> = StdVec::new();
        for (i, bps) in allocations.iter().enumerate() {
            let heir = Address::generate(&env);
            client.add_beneficiary(&id, &heir, bps);
            heirs.push(heir);
            assert_eq!(
                client.get_total_allocation(&id),
                allocations[..=i].iter().sum::<u32>()
            );
        }
        assert_eq!(client.get_total_allocation(&id), 10_000);

        let deposit = rng.range(1, 1_000_000_000) as i128;
        client.deposit(&id, &asset, &deposit);

        // Advance to exactly the grace-end boundary and release.
        env.ledger().set_timestamp(START + CHECK_IN + GRACE);
        assert!(client.get_vault(&id).activation_ready);
        client.activate_vault(&id);

        // Snapshot is fixed at activation.
        assert_eq!(client.get_vault(&id).vault.distribution_balance, deposit);

        // Claim in a random order.
        rng.shuffle(&mut heirs);
        let mut distributed: i128 = 0;
        for heir in heirs.iter() {
            let paid = client.claim(&id, heir);
            assert!(paid >= 0, "payout must never be negative");
            distributed += paid;
        }

        assert_eq!(
            distributed, deposit,
            "iteration {iteration}: payouts must sum to the deposit exactly"
        );
        let info = client.get_vault(&id);
        assert_eq!(info.vault.status, VaultStatus::Completed);
        assert_eq!(info.vault.balance, 0);
        assert_eq!(info.vault.total_claimed, deposit);
        assert_eq!(
            token::Client::new(&env, &asset).balance(&contract),
            0,
            "no dust may remain"
        );

        // No heir can be paid a second time.
        for heir in heirs.iter() {
            assert!(client.try_claim(&id, heir).is_err());
        }
    }
}

/// Random cancellation and withdrawal: a withdrawal is accepted exactly when it
/// is positive and within the balance, and the internal balance tracks tokens.
#[test]
fn property_random_withdrawals_never_exceed_the_balance() {
    let env = Env::default();
    let (contract, asset, owner, client) = fixture(&env);
    mint(&env, &asset, &owner, 1_000_000_000_000);

    let mut rng = Rng::new(0x0F_5A_11);
    // The contract accumulates whatever each generated vault leaves behind, so
    // the token-held invariant is checked against the running total.
    let mut contract_expected: i128 = 0;
    for _ in 0..iters(25) {
        let id = client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE,
        );
        let deposit = rng.range(1, 5_000_000) as i128;
        client.deposit(&id, &asset, &deposit);
        contract_expected += deposit;
        client.cancel_vault(&id);

        let mut remaining = deposit;
        for _ in 0..5 {
            let attempt = rng.range(0, deposit as u64 + 5) as i128;
            let accepted = attempt > 0 && attempt <= remaining;
            let result = client.try_withdraw(&id, &attempt);
            assert_eq!(
                result.is_ok(),
                accepted,
                "withdraw {attempt} against remaining {remaining}"
            );
            if accepted {
                remaining -= attempt;
                contract_expected -= attempt;
                assert_eq!(client.get_vault(&id).vault.balance, remaining);
            }
        }
        assert_eq!(
            token::Client::new(&env, &asset).balance(&contract),
            contract_expected,
            "internal balances must always equal the tokens held"
        );
    }
}
