//! Storage layout, schema versioning and the pagination index.
//!
//! # Layout
//!
//! HeirVault uses exactly four kinds of ledger entry, all keyed by
//! [`DataKey`]:
//!
//! | Key                          | Durability | Written by         | Purpose |
//! |------------------------------|------------|--------------------|---------|
//! | `NextVaultId`                | instance   | `create_vault`     | monotonic vault id allocator |
//! | `Vault(u64)`                 | persistent | most mutators      | the vault record itself |
//! | `OwnerVaultCount(Address)`   | persistent | `create_vault`     | size of an owner's vault list |
//! | `OwnerVault(Address, u32)`   | persistent | `create_vault`     | one slot of an owner's vault list |
//!
//! There is no `temporary` storage: every value a vault depends on must outlive
//! a single transaction, otherwise a vault could become permanently
//! unreadable.
//!
//! ## Instance vs persistent
//!
//! The vault-id counter lives in **instance** storage because it is a single
//! small value that almost every mutating call touches, and instance storage is
//! a single ledger entry that Soroban keeps resident for the duration of the
//! call. Vault records and the owner index live in **persistent** storage so
//! that each one carries its own rent and TTL; one owner's idle vault cannot
//! expire another owner's active one.
//!
//! ## Why an explicit owner index instead of a scanned collection
//!
//! `get_vaults_by_owner` must not scan every vault in the contract. Instead
//! `create_vault` appends one `OwnerVault(owner, i)` entry and bumps
//! `OwnerVaultCount(owner)`. A page is then `limit` point reads instead of a
//! full-collection scan, and the cost of a list call is independent of how many
//! vaults exist globally.
//!
//! The index is append-only. Vaults are never deleted, so slots are never
//! removed and offsets are stable across calls — a client can page without
//! worrying that an offset shifted underneath it.
//!
//! # Schema versioning and migration
//!
//! Two independent version markers exist:
//!
//! * [`crate::types::SCHEMA_VERSION`], a compile-time constant reported by
//!   `schema_version()`. It describes the layout the *running* code expects.
//! * `Vault::schema_version`, stamped into every vault when it is created. It
//!   describes the layout a *stored* record was written with.
//!
//! A future layout change therefore looks like: append fields to `Vault`, bump
//! `SCHEMA_VERSION`, add a `migrate_vault(id)` entry point that loads a record,
//! checks `vault.schema_version < SCHEMA_VERSION`, rewrites it in the new
//! layout and stamps the new version. Nothing has to be migrated eagerly and no
//! record becomes unreadable, because adding fields to a `#[contracttype]`
//! struct changes only the tail of the XDR, which the migration reads and
//! rewrites. See `docs/STORAGE.md` for the step-by-step procedure.

use soroban_sdk::{contracttype, Address, Env};

use crate::errors::HeirVaultError;
use crate::types::{PageMeta, Vault, MAX_PAGE_LIMIT};

/// Ledgers produced in a day at the ~5 second target close time.
const LEDGERS_PER_DAY: u32 = 17_280;

/// Extend a persistent entry's TTL once it drops below ~30 days.
const PERSISTENT_TTL_THRESHOLD: u32 = 30 * LEDGERS_PER_DAY;

/// ...out to ~90 days.
///
/// Comfortably below Soroban's maximum TTL, and long enough that an owner who
/// checks in less often than monthly never has to worry about a vault entry
/// being archived. Every read and write of a vault entry refreshes this, so an
/// actively used vault never expires.
const PERSISTENT_TTL_EXTEND_TO: u32 = 90 * LEDGERS_PER_DAY;

/// Extend the instance entry's TTL on every mutating call.
const INSTANCE_TTL_THRESHOLD: u32 = 30 * LEDGERS_PER_DAY;
const INSTANCE_TTL_EXTEND_TO: u32 = 90 * LEDGERS_PER_DAY;

/// Every ledger key HeirVault ever writes.
///
/// Deliberately a single enum: adding a variant is the only way to introduce a
/// new namespace, which keeps the whole layout discoverable from one place.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Next unallocated vault id. Instance storage.
    NextVaultId,
    /// A vault record, keyed by id. Persistent storage.
    Vault(u64),
    /// Number of vaults created by an owner. Persistent storage.
    OwnerVaultCount(Address),
    /// Slot `index` of an owner's vault list, holding a vault id. Persistent storage.
    OwnerVault(Address, u32),
    /// Number of vaults in which a beneficiary is or was enrolled. Persistent
    /// storage.
    BeneficiaryVaultCount(Address),

    /// Slot `index` of a beneficiary's vault list, holding a vault id. Persistent
    /// storage. Parallel append-only index to `OwnerVault`, keyed by the
    /// beneficiary address. Present so an heir or an indexer can enumerate every
    /// vault in which a given address is a beneficiary without scanning all
    /// vaults.
    BeneficiaryVault(Address, u32),
}

/// Refresh the instance entry's TTL.
///
/// Called from every mutating entry point. Cheap (one ledger entry) and it
/// guarantees the vault-id counter can never be archived out from under the
/// contract.
pub fn touch_instance(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
}

// ---------------------------------------------------------------------------
// Beneficiary vault index
// ---------------------------------------------------------------------------

/// Append `vault_id` to a beneficiary's vault list.
///
/// Called by `add_beneficiary` when a new slot is created. The index is
/// append-only: removing or deactivating a beneficiary never removes slots, so
/// offsets are stable across calls and a beneficiary page cannot shrink
/// underneath a client.
///
/// Slots are 0-indexed, matching the owner-side index (`push_owner_vault`): the
/// first slot lands at index 0, the count is then bumped to 1, and a page read
/// starting at offset 0 finds the first slot immediately.
pub fn append_beneficiary_vault(env: &Env, beneficiary: &Address, vault_id: u64) {
    let index = beneficiary_vault_count(env, beneficiary);

    let count_key = DataKey::BeneficiaryVaultCount(beneficiary.clone());
    let new_count = index
        .checked_add(1)
        .expect("beneficiary vault index must not overflow u32");
    env.storage()
        .persistent()
        .set(&count_key, &new_count);
    env.storage()
        .persistent()
        .extend_ttl(
            &count_key,
            PERSISTENT_TTL_THRESHOLD,
            PERSISTENT_TTL_EXTEND_TO,
        );

    let slot_key = DataKey::BeneficiaryVault(beneficiary.clone(), index);
    env.storage()
        .persistent()
        .set(&slot_key, &vault_id);
    env.storage()
        .persistent()
        .extend_ttl(
            &slot_key,
            PERSISTENT_TTL_THRESHOLD,
            PERSISTENT_TTL_EXTEND_TO,
        );
}

/// Number of vaults in which `beneficiary` is or was a beneficiary.
pub fn beneficiary_vault_count(env: &Env, beneficiary: &Address) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::BeneficiaryVaultCount(beneficiary.clone()))
        .unwrap_or(0)
}

/// Vault id at slot `index` of a beneficiary's vault list, or `None`.
pub fn beneficiary_vault_id_at(env: &Env, beneficiary: &Address, index: u32) -> Option<u64> {
    env.storage()
        .persistent()
        .get(&DataKey::BeneficiaryVault(beneficiary.clone(), index))
}

// ---------------------------------------------------------------------------
// Vault id allocator
// ---------------------------------------------------------------------------

/// The first vault id handed out. Ids start at 1 so that `0` is unambiguously
/// "no vault" in client code.
const FIRST_VAULT_ID: u64 = 1;

/// Allocate the next vault id and advance the counter.
///
/// Returns [`HeirVaultError::ArithmeticOverflow`] rather than wrapping, so an id
/// can never be handed out twice.
pub fn allocate_vault_id(env: &Env) -> Result<u64, HeirVaultError> {
    let current: u64 = env
        .storage()
        .instance()
        .get(&DataKey::NextVaultId)
        .unwrap_or(FIRST_VAULT_ID);
    let next = current
        .checked_add(1)
        .ok_or(HeirVaultError::ArithmeticOverflow)?;
    env.storage().instance().set(&DataKey::NextVaultId, &next);
    Ok(current)
}

/// How many ids have been handed out (vaults are never deleted, so this is the
/// exact number of vaults that exist).
pub fn vault_count(env: &Env) -> u64 {
    let next: u64 = env
        .storage()
        .instance()
        .get(&DataKey::NextVaultId)
        .unwrap_or(FIRST_VAULT_ID);
    next.saturating_sub(FIRST_VAULT_ID)
}

// ---------------------------------------------------------------------------
// Vault records
// ---------------------------------------------------------------------------

/// `true` if a vault record exists at `id`.
pub fn has_vault(env: &Env, id: u64) -> bool {
    env.storage().persistent().has(&DataKey::Vault(id))
}

/// Load a vault, refreshing its TTL, or `None` if no record is readable.
///
/// Used by list endpoints, which skip an unreadable slot rather than failing an
/// entire page. Unlike `get_vault` there is no way to distinguish "never
/// created" from "archived by the network" here — Soroban's persistent-storage
/// API does not expose the reason for a miss — so callers must treat `None` as
/// "no readable vault at this id".
pub fn load_vault_opt(env: &Env, id: u64) -> Option<Vault> {
    let key = DataKey::Vault(id);
    let vault: Option<Vault> = env.storage().persistent().get(&key);
    if vault.is_some() {
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_TTL_THRESHOLD,
            PERSISTENT_TTL_EXTEND_TO,
        );
    }
    vault
}

/// Load a vault, refreshing its TTL, or fail with [`HeirVaultError::VaultNotFound`].
pub fn load_vault(env: &Env, id: u64) -> Result<Vault, HeirVaultError> {
    load_vault_opt(env, id).ok_or(HeirVaultError::VaultNotFound)
}

/// Write a vault record and refresh its TTL.
pub fn save_vault(env: &Env, vault: &Vault) {
    let key = DataKey::Vault(vault.id);
    env.storage().persistent().set(&key, vault);
    env.storage()
        .persistent()
        .extend_ttl(&key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_EXTEND_TO);
}

// ---------------------------------------------------------------------------
// Owner index (drives `get_vaults_by_owner`)
// ---------------------------------------------------------------------------

/// Number of vaults recorded against `owner`.
pub fn owner_vault_count(env: &Env, owner: &Address) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::OwnerVaultCount(owner.clone()))
        .unwrap_or(0)
}

/// Append `vault_id` to `owner`'s list.
///
/// Two writes: the new slot, and the grown count. Both are point writes, so the
/// cost of creating an owner's tenth vault is the same as their first.
pub fn push_owner_vault(env: &Env, owner: &Address, vault_id: u64) -> Result<(), HeirVaultError> {
    let index = owner_vault_count(env, owner);
    let new_count = index
        .checked_add(1)
        .ok_or(HeirVaultError::ArithmeticOverflow)?;

    let count_key = DataKey::OwnerVaultCount(owner.clone());
    env.storage().persistent().set(&count_key, &new_count);
    env.storage().persistent().extend_ttl(
        &count_key,
        PERSISTENT_TTL_THRESHOLD,
        PERSISTENT_TTL_EXTEND_TO,
    );

    let slot_key = DataKey::OwnerVault(owner.clone(), index);
    env.storage().persistent().set(&slot_key, &vault_id);
    env.storage().persistent().extend_ttl(
        &slot_key,
        PERSISTENT_TTL_THRESHOLD,
        PERSISTENT_TTL_EXTEND_TO,
    );
    Ok(())
}

/// Read one slot of `owner`'s vault list, or `None` past the end.
pub fn owner_vault_id_at(env: &Env, owner: &Address, index: u32) -> Option<u64> {
    env.storage()
        .persistent()
        .get(&DataKey::OwnerVault(owner.clone(), index))
}

// ---------------------------------------------------------------------------
// Paging helpers
// ---------------------------------------------------------------------------

/// Validate a caller-supplied page limit.
///
/// A limit of zero is rejected (it can only be a client bug). A limit above
/// [`MAX_PAGE_LIMIT`] is clamped rather than rejected, so a client that asks for
/// "everything" still gets a bounded, simulatable result instead of an error it
/// cannot act on.
pub fn clamp_limit(limit: u32) -> Result<u32, HeirVaultError> {
    if limit == 0 {
        return Err(HeirVaultError::InvalidPageLimit);
    }
    Ok(limit.min(MAX_PAGE_LIMIT))
}

/// Build the [`PageMeta`] block for a page that returned `returned` records.
///
/// Centralised so every list endpoint reports paging identically: the same
/// arithmetic drives `has_more` and `next_offset`, which is what lets an SDK
/// write one paging loop for every collection.
pub fn page_meta(total: u32, offset: u32, limit: u32, returned: u32) -> PageMeta {
    let consumed = offset.saturating_add(returned);
    let has_more = consumed < total;
    PageMeta {
        total,
        offset,
        limit,
        has_more,
        next_offset: if has_more { Some(consumed) } else { None },
    }
}
