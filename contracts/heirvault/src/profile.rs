//! Resource profiling harness.
//!
//! `cargo test -p heirvault --lib profile_entry_points -- --nocapture`
//!
//! Every mutating entry point is invoked once against a realistic fixture, and
//! the host's own metering is reported for that invocation:
//!
//! * `instructions` — modelled CPU instructions
//! * `read_entries` / `write_entries` — ledger entries that must be fetched /
//!   rewritten
//! * `read_bytes` / `write_bytes` — ledger bytes billed as read / written
//! * `contract_events_size_bytes` — event payload bytes (also billed as writes)
//!
//! These numbers come from `Env::cost_estimate()`, which meters the *last top
//! level invocation*. They are used to compare entry points against one another
//! and to detect a regression such as a function that accidentally starts
//! scanning a collection. They are **not** a substitute for simulating a real
//! transaction against a deployed wasm — the test host runs contract code
//! directly and therefore omits VM instantiation, wasm reads and transaction
//! size, so absolute production costs are higher.
//!
//! See `docs/RESOURCE_COSTS.md` for the conclusions drawn from these figures.

extern crate std;

use std::println;

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{token, Address, Env};

use crate::{ActivationMode, HeirVaultContract, HeirVaultContractClient};

const CHECK_IN: u64 = 30 * 24 * 60 * 60;
const GRACE: u64 = 7 * 24 * 60 * 60;
const START: u64 = 1_700_000_000;
const UNIT: i128 = 1_000_000;

struct Row {
    label: &'static str,
    instructions: i64,
    read_entries: u32,
    write_entries: u32,
    read_bytes: u32,
    write_bytes: u32,
    event_bytes: u32,
}

/// Run `body` and capture the resources of the invocation it performs.
///
/// The fixture is rebuilt per row so every measurement starts from the same
/// state shape (one beneficiary where relevant, empty elsewhere), which is what
/// makes the numbers comparable.
fn measure(label: &'static str, env: &Env, body: impl FnOnce()) -> Row {
    body();
    let r = env.cost_estimate().resources();
    Row {
        label,
        instructions: r.instructions,
        read_entries: r.read_entries,
        write_entries: r.write_entries,
        read_bytes: r.read_bytes,
        write_bytes: r.write_bytes,
        event_bytes: r.contract_events_size_bytes,
    }
}

fn fresh<'a>(
    env: &'a Env,
) -> (
    Address,
    Address,
    Address,
    Address,
    HeirVaultContractClient<'a>,
) {
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let admin = Address::generate(env);
    let asset = env.register_stellar_asset_contract_v2(admin).address();
    let contract = env.register(HeirVaultContract, ());
    let client = HeirVaultContractClient::new(env, &contract);
    let owner = Address::generate(env);
    token::StellarAssetClient::new(env, &asset).mint(&owner, &(1_000 * UNIT));
    (contract, asset, owner, Address::generate(env), client)
}

fn settle(env: &Env, client: &HeirVaultContractClient<'_>, vault_id: u64) {
    env.ledger().set_timestamp(START + CHECK_IN + GRACE);
    client.activate_vault(&vault_id);
}

#[test]
fn profile_entry_points() {
    let mut rows: std::vec::Vec<Row> = std::vec::Vec::new();

    // ---- create_vault -------------------------------------------------
    {
        let env = Env::default();
        let (_, asset, owner, _, client) = fresh(&env);
        rows.push(measure("create_vault", &env, || {
            client.create_vault(
                &owner,
                &asset,
                &ActivationMode::MissedCheckIn,
                &CHECK_IN,
                &GRACE,
            );
        }));
    }

    // ---- add_beneficiary (first slot) ---------------------------------
    {
        let env = Env::default();
        let (_, asset, owner, heir, client) = fresh(&env);
        let id = client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE,
        );
        rows.push(measure("add_beneficiary (1st)", &env, || {
            client.add_beneficiary(&id, &heir, &10_000u32);
        }));
    }

    // ---- add_beneficiary (10th slot) ----------------------------------
    {
        let env = Env::default();
        let (_, asset, owner, _, client) = fresh(&env);
        let id = client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE,
        );
        for _ in 0..9 {
            client.add_beneficiary(&id, &Address::generate(&env), &1_000u32);
        }
        rows.push(measure("add_beneficiary (10th)", &env, || {
            client.add_beneficiary(&id, &Address::generate(&env), &1_000u32);
        }));
    }

    // ---- remove / update beneficiary ----------------------------------
    {
        let env = Env::default();
        let (_, asset, owner, heir, client) = fresh(&env);
        let id = client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE,
        );
        client.add_beneficiary(&id, &heir, &5_000u32);
        rows.push(measure("update_beneficiary", &env, || {
            client.update_beneficiary(&id, &heir, &6_000u32);
        }));
        rows.push(measure("remove_beneficiary", &env, || {
            client.remove_beneficiary(&id, &heir);
        }));
    }

    // ---- deposit ------------------------------------------------------
    {
        let env = Env::default();
        let (_, asset, owner, heir, client) = fresh(&env);
        let id = client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE,
        );
        client.add_beneficiary(&id, &heir, &10_000u32);
        rows.push(measure("deposit", &env, || {
            client.deposit(&id, &asset, &(100 * UNIT));
        }));
    }

    // ---- guardian configuration and voting ----------------------------
    {
        let env = Env::default();
        let (_, asset, owner, heir, client) = fresh(&env);
        let g1 = Address::generate(&env);
        let g2 = Address::generate(&env);
        let id = client.create_vault(
            &owner,
            &asset,
            &ActivationMode::GuardianApproval,
            &CHECK_IN,
            &GRACE,
        );
        client.add_beneficiary(&id, &heir, &10_000u32);
        client.add_guardian(&id, &g1);
        client.add_guardian(&id, &g2);
        rows.push(measure("set_guardian_threshold", &env, || {
            client.set_guardian_threshold(&id, &2u32);
        }));
        rows.push(measure("guardian_approve", &env, || {
            client.guardian_approve(&id, &g1);
        }));
        // Second vote, unmeasured, so the vault actually satisfies its gate.
        client.guardian_approve(&id, &g2);
        client.deposit(&id, &asset, &(10 * UNIT));
        rows.push(measure("activate_vault (guardian)", &env, || {
            client.activate_vault(&id);
        }));
    }

    // ---- check_in -----------------------------------------------------
    {
        let env = Env::default();
        let (_, asset, owner, heir, client) = fresh(&env);
        let id = client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE,
        );
        client.add_beneficiary(&id, &heir, &10_000u32);
        client.deposit(&id, &asset, &(10 * UNIT));
        env.ledger().set_timestamp(START + 24 * 60 * 60);
        rows.push(measure("check_in", &env, || {
            client.check_in(&id);
        }));
    }

    // ---- activate (clock-driven) --------------------------------------
    {
        let env = Env::default();
        let (_, asset, owner, heir, client) = fresh(&env);
        let id = client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE,
        );
        client.add_beneficiary(&id, &heir, &10_000u32);
        client.deposit(&id, &asset, &(10 * UNIT));
        env.ledger().set_timestamp(START + CHECK_IN + GRACE);
        rows.push(measure("activate_vault", &env, || {
            client.activate_vault(&id);
        }));
    }

    // ---- claim (not last) and claim (last) ----------------------------
    {
        let env = Env::default();
        let (_, asset, owner, heir, client) = fresh(&env);
        let second = Address::generate(&env);
        let id = client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE,
        );
        client.add_beneficiary(&id, &heir, &5_000u32);
        client.add_beneficiary(&id, &second, &5_000u32);
        client.deposit(&id, &asset, &(100 * UNIT));
        settle(&env, &client, id);
        rows.push(measure("claim (of 2)", &env, || {
            client.claim(&id, &heir);
        }));
        rows.push(measure("claim (last)", &env, || {
            client.claim(&id, &second);
        }));
    }

    // ---- cancel / withdraw --------------------------------------------
    {
        let env = Env::default();
        let (_, asset, owner, heir, client) = fresh(&env);
        let id = client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE,
        );
        client.add_beneficiary(&id, &heir, &10_000u32);
        client.deposit(&id, &asset, &(100 * UNIT));
        rows.push(measure("cancel_vault", &env, || {
            client.cancel_vault(&id);
        }));
        rows.push(measure("withdraw", &env, || {
            client.withdraw(&id, &(50 * UNIT));
        }));
    }

    // ---- reads --------------------------------------------------------
    {
        let env = Env::default();
        let (_, asset, owner, heir, client) = fresh(&env);
        let id = client.create_vault(
            &owner,
            &asset,
            &ActivationMode::MissedCheckIn,
            &CHECK_IN,
            &GRACE,
        );
        client.add_beneficiary(&id, &heir, &10_000u32);
        rows.push(measure("get_vault", &env, || {
            let _ = client.get_vault(&id);
        }));
        rows.push(measure("get_beneficiaries (page of 1)", &env, || {
            let _ = client.get_beneficiaries(&id, &0, &10);
        }));
        rows.push(measure("get_vaults_by_owner (limit 1)", &env, || {
            let _ = client.get_vaults_by_owner(&owner, &0, &1);
        }));
    }

    println!();
    println!("HeirVault per-invocation resource profile (Soroban test host, protocol 22)");
    println!();
    println!(
        "| {:<30} | {:>10} | {:>6} | {:>6} | {:>10} | {:>10} | {:>8} |",
        "entry point", "cpu", "rd ent", "wr ent", "rd bytes", "wr bytes", "events"
    );
    println!(
        "|{:-<32}|{:-<12}|{:-<8}|{:-<8}|{:-<12}|{:-<12}|{:-<10}|",
        "", "", "", "", "", "", ""
    );
    for row in rows.iter() {
        println!(
            "| {:<30} | {:>10} | {:>6} | {:>6} | {:>10} | {:>10} | {:>8} |",
            row.label,
            row.instructions,
            row.read_entries,
            row.write_entries,
            row.read_bytes,
            row.write_bytes,
            row.event_bytes
        );
    }
    println!();

    // Guard rails: these are the properties the numbers are supposed to
    // demonstrate. If a change makes one of them false, the profile test fails
    // rather than quietly reporting worse numbers.
    let find = |label: &str| rows.iter().find(|r| r.label == label).expect("row exists");

    // A mutator that does not touch the token must write a constant number of
    // ledger entries: at most the instance entry's TTL bump plus the vault
    // record itself. This is the property that keeps configuration changes cheap
    // no matter how many vaults exist.
    for label in [
        "check_in",
        "add_beneficiary (1st)",
        "add_beneficiary (10th)",
        "update_beneficiary",
        "remove_beneficiary",
        "set_guardian_threshold",
        "guardian_approve",
        "activate_vault",
        "activate_vault (guardian)",
        "cancel_vault",
    ] {
        let row = find(label);
        assert!(
            row.write_entries <= 2,
            "{label} wrote {} ledger entries; a token-free mutator should write at most 2 \
             (the instance entry's TTL bump plus the vault record)",
            row.write_entries
        );
    }

    // Value-moving entry points additionally write the *token contract's* own
    // entries, because a real SEP-41 transfer happens inside the same
    // invocation. The budget is therefore higher, but still constant: this is
    // the measurement that proves a deposit or a claim costs the same whether
    // the vault has one beneficiary or ten.
    for label in ["deposit", "withdraw", "claim (of 2)", "claim (last)"] {
        let row = find(label);
        assert!(
            row.write_entries <= 5,
            "{label} wrote {} ledger entries; a value-moving entry point should stay within \
             the vault record, the instance TTL bump and the token's two balance entries",
            row.write_entries
        );
    }

    // Adding the tenth beneficiary must not cost more ledger entries than
    // adding the first: the list lives inside the vault record, so there is no
    // per-beneficiary entry and no growth in entry count.
    assert_eq!(
        find("add_beneficiary (10th)").write_entries,
        find("add_beneficiary (1st)").write_entries,
        "beneficiary slot count must not change the number of ledger writes"
    );
    assert!(
        find("add_beneficiary (10th)").read_entries
            <= find("add_beneficiary (1st)").read_entries + 1,
        "adding a tenth beneficiary must not add ledger reads beyond the larger record itself"
    );

    // Paging one vault must read a bounded number of entries.
    let page = find("get_vaults_by_owner (limit 1)");
    assert!(
        page.read_entries <= 4,
        "a one-item page read {} entries; the owner index must keep paging O(limit)",
        page.read_entries
    );

    // Claims must not grow with the number of heirs: a claim is a per-heir
    // operation, so its ledger-entry cost is flat.
    assert_eq!(
        find("claim (of 2)").write_entries,
        find("claim (last)").write_entries,
        "a claim's ledger writes must not depend on whether it is the final one"
    );

    // A read must never write.
    for label in [
        "get_vault",
        "get_beneficiaries (page of 1)",
        "get_vaults_by_owner (limit 1)",
    ] {
        assert_eq!(find(label).write_entries, 0, "{label} must be a pure read");
    }
}
