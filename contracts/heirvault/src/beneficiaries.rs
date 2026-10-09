//! Beneficiary management: who inherits, and how much.
//!
//! # Allocation model
//!
//! Shares are stored in basis points (1/100th of a percent), so the unit of
//! account is an integer and there is no floating-point drift anywhere in the
//! contract. The invariant enforced by every mutator in this module is:
//!
//! ```text
//! sum(beneficiary.allocation_bps where active) <= 10_000
//! ```
//!
//! The strict form of the invariant, `== 10_000`, is *not* required while the
//! vault is being edited — an owner needs to be able to build up a list one
//! entry at a time, and to remove an entry without being forced to immediately
//! rebalance. It is enforced once, at the moment it starts to matter, by
//! `activate_vault`, which refuses to release a vault whose allocations do not
//! sum to exactly 100%. That gives a single, auditable rule:
//!
//! > **Every activated vault is exactly 100% allocated.**
//!
//! # Slots are retained, not deleted
//!
//! `remove_beneficiary` deactivates a slot (`active = false`, allocation zeroed)
//! rather than deleting it from the list. Three consequences, all deliberate:
//!
//! 1. The on-chain record of who was ever entitled to a vault survives removal,
//!    which matters for an inheritance product where a dispute years later may
//!    hinge on the configuration at a point in time.
//! 2. Re-adding the same address reactivates the existing slot instead of
//!    creating a second one, so an address can never appear twice in a vault.
//! 3. The vector is capped at [`MAX_BENEFICIARIES`] *slots*, so a vault is
//!    capped at that many distinct addresses over its lifetime. Owners who need
//!    more history create another vault.

use soroban_sdk::{Address, Env, Vec};

use crate::errors::HeirVaultError;
use crate::events;
use crate::storage;
use crate::types::{Beneficiary, BeneficiaryPage, Vault, FULL_ALLOCATION_BPS, MAX_BENEFICIARIES};
use crate::vault;

/// Index of a slot for `address` regardless of whether it is active.
fn slot_index(vault: &Vault, address: &Address) -> Option<u32> {
    for (index, slot) in vault.beneficiaries.iter().enumerate() {
        if &slot.address == address {
            return Some(index as u32);
        }
    }
    None
}

/// Add a beneficiary, or reactivate a previously removed one.
///
/// Rejects: a non-owner caller, an uneditable vault, a zero or >100% share, a
/// share that would push the total above 100%, a duplicate address, an address
/// equal to the owner or to the contract itself, and a list already at
/// [`MAX_BENEFICIARIES`] slots.
pub fn add(
    env: &Env,
    vault_id: u64,
    beneficiary: Address,
    allocation_bps: u32,
) -> Result<(), HeirVaultError> {
    let mut vault = storage::load_vault(env, vault_id)?;
    vault::require_owner(&vault);
    vault::sync_status(env, &mut vault);
    vault::require_editable(&vault)?;

    if allocation_bps == 0 || allocation_bps > FULL_ALLOCATION_BPS {
        return Err(HeirVaultError::InvalidAllocation);
    }
    vault::require_valid_party(env, &vault, &beneficiary)?;

    // A currently active duplicate is rejected outright rather than silently
    // overwriting the existing share.
    if vault.beneficiary_index(&beneficiary).is_some() {
        return Err(HeirVaultError::DuplicateBeneficiary);
    }

    // The structural cap is reported before the allocation ceiling: "you already
    // have ten heirs" is the more actionable error when both would apply.
    let existing = slot_index(&vault, &beneficiary);
    if existing.is_none() && vault.beneficiaries.len() >= MAX_BENEFICIARIES {
        return Err(HeirVaultError::TooManyBeneficiaries);
    }

    let new_total = vault
        .total_allocation_bps()
        .checked_add(allocation_bps)
        .ok_or(HeirVaultError::ArithmeticOverflow)?;
    if new_total > FULL_ALLOCATION_BPS {
        return Err(HeirVaultError::AllocationExceeded);
    }

    match existing {
        // Reactivate the retained slot so the address appears exactly once.
        Some(index) => {
            let mut slot = vault
                .beneficiaries
                .get(index)
                .ok_or(HeirVaultError::InvalidBeneficiary)?;
            slot.active = true;
            slot.allocation_bps = allocation_bps;
            slot.claimed = false;
            slot.claimed_amount = 0;
            vault.beneficiaries.set(index, slot);
        }
        None => {
            vault.beneficiaries.push_back(Beneficiary {
                address: beneficiary.clone(),
                allocation_bps,
                active: true,
                claimed: false,
                claimed_amount: 0,
            });
        }
    }

    storage::save_vault(env, &vault);
    events::beneficiary_added(
        env,
        vault_id,
        &beneficiary,
        allocation_bps,
        vault.total_allocation_bps(),
    );
    Ok(())
}

/// Deactivate a beneficiary slot and release its allocation.
///
/// Removing frees the address for re-adding and lowers the total allocation; it
/// does not require the remaining shares to sum to 100%, because that is only
/// enforced at activation.
pub fn remove(env: &Env, vault_id: u64, beneficiary: Address) -> Result<(), HeirVaultError> {
    let mut vault = storage::load_vault(env, vault_id)?;
    vault::require_owner(&vault);
    vault::sync_status(env, &mut vault);
    vault::require_editable(&vault)?;

    let index = vault
        .beneficiary_index(&beneficiary)
        .ok_or(HeirVaultError::InvalidBeneficiary)?;

    let mut slot = vault
        .beneficiaries
        .get(index)
        .ok_or(HeirVaultError::InvalidBeneficiary)?;
    slot.active = false;
    slot.allocation_bps = 0;
    vault.beneficiaries.set(index, slot);

    storage::save_vault(env, &vault);
    events::beneficiary_removed(env, vault_id, &beneficiary, vault.total_allocation_bps());
    Ok(())
}

/// Change an active beneficiary's allocation.
///
/// The new share is checked against the *other* beneficiaries' total, so the
/// 100% ceiling holds no matter how the share moves.
pub fn update(
    env: &Env,
    vault_id: u64,
    beneficiary: Address,
    new_allocation_bps: u32,
) -> Result<(), HeirVaultError> {
    let mut vault = storage::load_vault(env, vault_id)?;
    vault::require_owner(&vault);
    vault::sync_status(env, &mut vault);
    vault::require_editable(&vault)?;

    if new_allocation_bps == 0 || new_allocation_bps > FULL_ALLOCATION_BPS {
        return Err(HeirVaultError::InvalidAllocation);
    }

    let index = vault
        .beneficiary_index(&beneficiary)
        .ok_or(HeirVaultError::InvalidBeneficiary)?;

    let mut slot = vault
        .beneficiaries
        .get(index)
        .ok_or(HeirVaultError::InvalidBeneficiary)?;
    let previous = slot.allocation_bps;

    let others = vault
        .total_allocation_bps()
        .checked_sub(previous)
        .ok_or(HeirVaultError::ArithmeticOverflow)?;
    let new_total = others
        .checked_add(new_allocation_bps)
        .ok_or(HeirVaultError::ArithmeticOverflow)?;
    if new_total > FULL_ALLOCATION_BPS {
        return Err(HeirVaultError::AllocationExceeded);
    }

    slot.allocation_bps = new_allocation_bps;
    vault.beneficiaries.set(index, slot);

    storage::save_vault(env, &vault);
    events::beneficiary_updated(
        env,
        vault_id,
        &beneficiary,
        previous,
        new_allocation_bps,
        vault.total_allocation_bps(),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// Total active allocation in basis points.
pub fn total_allocation(env: &Env, vault_id: u64) -> Result<u32, HeirVaultError> {
    let vault = storage::load_vault(env, vault_id)?;
    Ok(vault.total_allocation_bps())
}

/// One page of beneficiary slots.
///
/// Returns **every** slot including deactivated ones, so a client can render
/// history as well as the current list; filter on [`Beneficiary::active`] for
/// the live set. Offsets are stable because slots are never compacted away.
pub fn get_page(
    env: &Env,
    vault_id: u64,
    offset: u32,
    limit: u32,
) -> Result<BeneficiaryPage, HeirVaultError> {
    let vault = storage::load_vault(env, vault_id)?;
    let limit = storage::clamp_limit(limit)?;
    let total = vault.beneficiaries.len();

    let mut items: Vec<Beneficiary> = Vec::new(env);
    let mut cursor = offset;
    while cursor < total && items.len() < limit {
        if let Some(slot) = vault.beneficiaries.get(cursor) {
            items.push_back(slot);
        }
        cursor += 1;
    }

    let meta = storage::page_meta(total, offset, limit, items.len());
    Ok(BeneficiaryPage { items, meta })
}

/// Every active beneficiary address, unpaged.
///
/// Bounded by [`MAX_BENEFICIARIES`], so a single read is always safe. Provided
/// for callers that need the whole live set (for example an off-chain
/// distribution preview) without writing a paging loop.
pub fn get_active(env: &Env, vault_id: u64) -> Result<Vec<Beneficiary>, HeirVaultError> {
    let vault = storage::load_vault(env, vault_id)?;
    let mut items: Vec<Beneficiary> = Vec::new(env);
    for slot in vault.beneficiaries.iter() {
        if slot.active {
            items.push_back(slot);
        }
    }
    Ok(items)
}
