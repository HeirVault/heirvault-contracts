//! HeirVault event system.
//!
//! # Conventions
//!
//! * Topic `[0]` is a stable `Symbol` naming the event, e.g. `"vault_created"`.
//! * Topic `[1]` is the vault id, so a subscriber can filter one vault's whole
//!   history from a single ledger range.
//! * The data payload carries everything else an indexer needs.
//!
//! Event names, topic positions and payload field order are part of the public
//! ABI. Append payload fields; never reorder or rename existing ones.
//!
//! Long symbols (`"beneficiary_removed"`) are built with [`Symbol::new`] rather
//! than `symbol_short!`, which is limited to nine characters. The cost of one
//! symbol allocation per emitting call is negligible next to the storage writes
//! the same call performs, and it buys unambiguous, self-documenting names.
//!
//! # Payload discipline
//!
//! Payloads carry *identifiers and deltas*, not reconstructed state. A client
//! that needs the full vault after an event reads it back with `get_vault`
//! rather than expecting the event to contain it. This keeps event bytes (which
//! are charged as ledger writes) small and bounded regardless of how many
//! beneficiaries a vault has.

use soroban_sdk::{Address, Env, Symbol};

use crate::types::ActivationMode;

/// Publish an event whose topics are `(name, vault_id)`.
fn emit<D>(env: &Env, name: &str, vault_id: u64, data: D)
where
    D: soroban_sdk::IntoVal<Env, soroban_sdk::Val>,
{
    let topics = (Symbol::new(env, name), vault_id);
    env.events().publish(topics, data);
}

/// A vault was created. Emitted by `create_vault`.
pub fn vault_created(
    env: &Env,
    vault_id: u64,
    owner: &Address,
    asset: &Address,
    activation_mode: ActivationMode,
    check_in_period: u64,
    grace_period: u64,
) {
    emit(
        env,
        "vault_created",
        vault_id,
        (
            owner.clone(),
            asset.clone(),
            activation_mode,
            check_in_period,
            grace_period,
            env.ledger().timestamp(),
        ),
    );
}

/// The owner funded the vault. Emitted by `deposit`.
pub fn deposit_made(env: &Env, vault_id: u64, depositor: &Address, amount: i128, balance: i128) {
    emit(
        env,
        "deposit_made",
        vault_id,
        (depositor.clone(), amount, balance),
    );
}

/// A beneficiary slot was added or reactivated. Emitted by `add_beneficiary`.
pub fn beneficiary_added(
    env: &Env,
    vault_id: u64,
    beneficiary: &Address,
    allocation_bps: u32,
    total_allocation_bps: u32,
) {
    emit(
        env,
        "beneficiary_added",
        vault_id,
        (beneficiary.clone(), allocation_bps, total_allocation_bps),
    );
}

/// A beneficiary slot was deactivated. Emitted by `remove_beneficiary`.
pub fn beneficiary_removed(
    env: &Env,
    vault_id: u64,
    beneficiary: &Address,
    total_allocation_bps: u32,
) {
    emit(
        env,
        "beneficiary_removed",
        vault_id,
        (beneficiary.clone(), total_allocation_bps),
    );
}

/// A beneficiary's allocation changed. Emitted by `update_beneficiary`.
pub fn beneficiary_updated(
    env: &Env,
    vault_id: u64,
    beneficiary: &Address,
    old_allocation_bps: u32,
    new_allocation_bps: u32,
    total_allocation_bps: u32,
) {
    emit(
        env,
        "beneficiary_updated",
        vault_id,
        (
            beneficiary.clone(),
            old_allocation_bps,
            new_allocation_bps,
            total_allocation_bps,
        ),
    );
}

/// A guardian slot was added or reactivated. Emitted by `add_guardian`.
pub fn guardian_added(
    env: &Env,
    vault_id: u64,
    guardian: &Address,
    guardian_count: u32,
    threshold: u32,
) {
    emit(
        env,
        "guardian_added",
        vault_id,
        (guardian.clone(), guardian_count, threshold),
    );
}

/// A guardian slot was deactivated. Emitted by `remove_guardian`.
pub fn guardian_removed(
    env: &Env,
    vault_id: u64,
    guardian: &Address,
    guardian_count: u32,
    threshold: u32,
) {
    emit(
        env,
        "guardian_removed",
        vault_id,
        (guardian.clone(), guardian_count, threshold),
    );
}

/// The approval threshold changed. Emitted by `set_guardian_threshold`.
pub fn guardian_threshold_updated(env: &Env, vault_id: u64, threshold: u32, guardian_count: u32) {
    emit(
        env,
        "guardian_threshold_updated",
        vault_id,
        (threshold, guardian_count),
    );
}

/// A guardian voted on the current activation attempt. Emitted by `guardian_approve`.
pub fn guardian_approved(
    env: &Env,
    vault_id: u64,
    guardian: &Address,
    approvals: u32,
    threshold: u32,
) {
    emit(
        env,
        "guardian_approved",
        vault_id,
        (guardian.clone(), approvals, threshold),
    );
}

/// The owner checked in. Emitted by `check_in`.
pub fn check_in_completed(
    env: &Env,
    vault_id: u64,
    owner: &Address,
    last_check_in: u64,
    next_deadline: u64,
) {
    emit(
        env,
        "check_in_completed",
        vault_id,
        (owner.clone(), last_check_in, next_deadline),
    );
}

/// Inheritance was triggered. Emitted by `activate_vault`.
pub fn vault_activated(
    env: &Env,
    vault_id: u64,
    activation_mode: ActivationMode,
    distribution_balance: i128,
    beneficiary_count: u32,
) {
    emit(
        env,
        "vault_activated",
        vault_id,
        (
            activation_mode,
            distribution_balance,
            beneficiary_count,
            env.ledger().timestamp(),
        ),
    );
}

/// A beneficiary withdrew its allocation. Emitted by `claim`.
pub fn inheritance_claimed(
    env: &Env,
    vault_id: u64,
    beneficiary: &Address,
    amount: i128,
    total_claimed: i128,
    remaining: i128,
) {
    emit(
        env,
        "inheritance_claimed",
        vault_id,
        (
            beneficiary.clone(),
            amount,
            total_claimed,
            remaining,
            env.ledger().timestamp(),
        ),
    );
}

/// The owner cancelled the vault. Emitted by `cancel_vault`.
pub fn vault_cancelled(env: &Env, vault_id: u64, owner: &Address, refundable_balance: i128) {
    emit(
        env,
        "vault_cancelled",
        vault_id,
        (owner.clone(), refundable_balance),
    );
}

/// The owner withdrew from a cancelled vault. Emitted by `withdraw`.
pub fn withdrawal(
    env: &Env,
    vault_id: u64,
    owner: &Address,
    amount: i128,
    remaining_balance: i128,
) {
    emit(
        env,
        "withdrawal",
        vault_id,
        (owner.clone(), amount, remaining_balance),
    );
}

/// Every beneficiary has claimed. Emitted by `claim` when the vault finishes.
pub fn vault_completed(env: &Env, vault_id: u64, total_distributed: i128, beneficiary_count: u32) {
    emit(
        env,
        "vault_completed",
        vault_id,
        (
            total_distributed,
            beneficiary_count,
            env.ledger().timestamp(),
        ),
    );
}
