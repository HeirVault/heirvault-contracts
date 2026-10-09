//! Guardian management: M-of-N approval over a vault release.
//!
//! # Model
//!
//! A vault may name up to [`MAX_GUARDIANS`] guardians and require an
//! independent, configurable [`Vault::guardian_threshold`] of them to approve
//! before the vault can be released. Guardians are an *additional* release path
//! for the case where the owner is incapacitated rather than merely inactive —
//! they are not a replacement for the check-in mechanism, and a vault's
//! [`ActivationMode`] decides whether they are consulted at all:
//!
//! | Activation mode    | Guardian approval used? | Missed check-in used? |
//! |--------------------|-------------------------|-----------------------|
//! | `MissedCheckIn`    | no                      | yes                   |
//! | `GuardianApproval` | yes                     | no                    |
//! | `MultiCondition`   | yes                     | yes                   |
//!
//! `guardian_approve` on a `MissedCheckIn` vault is rejected with
//! [`HeirVaultError::ApprovalNotEnabled`] rather than silently recorded, so a
//! vault can never accumulate votes that mean nothing.
//!
//! # Approval lifecycle
//!
//! * An approval is a `bool` on the guardian's own slot, never a counter the
//!   contract increments blindly. `guardian_approvals` is the cached count of
//!   those flags.
//! * A guardian may approve **once per activation attempt**; a second call is
//!   [`HeirVaultError::AlreadyApproved`]. This is what makes the count
//!   meaningful: the threshold is a count of *distinct* guardians.
//! * Every owner check-in clears the flag on every guardian slot and resets the
//!   counter. A vote cast before the owner proved they were alive can therefore
//!   never be replayed against a later attempt.
//!
//! # Anti-lockout invariant
//!
//! Removing a guardian, and lowering the threshold, are both refused when they
//! would leave a guardian-gated vault unable to *ever* satisfy its activation
//! conditions — no guardians left, or a zero threshold. Without that check an
//! owner could (accidentally or maliciously) strand a vault's funds where no
//! beneficiary could ever claim them, which is exactly the failure mode an
//! inheritance contract exists to prevent.

use soroban_sdk::{Address, Env, Vec};

use crate::errors::HeirVaultError;
use crate::events;
use crate::storage;
use crate::types::{Guardian, GuardianPage, GuardianStatus, Vault, MAX_GUARDIANS};
use crate::vault;

/// Index of a guardian slot for `address`, active or not.
fn slot_index(vault: &Vault, address: &Address) -> Option<u32> {
    for (index, slot) in vault.guardians.iter().enumerate() {
        if &slot.address == address {
            return Some(index as u32);
        }
    }
    None
}

/// Index of the active guardian slot for `address`.
fn active_slot_index(vault: &Vault, address: &Address) -> Option<u32> {
    vault
        .guardians
        .iter()
        .enumerate()
        .find(|(_, slot)| slot.active && &slot.address == address)
        .map(|(index, _)| index as u32)
}

/// Default threshold applied when the first guardian is added.
///
/// One approval from one guardian is the only sensible starting point: the
/// owner can always raise it with `set_guardian_threshold`, and raising is a
/// deliberate act whereas a default of "impossible" would silently break a
/// `GuardianApproval` vault.
const DEFAULT_THRESHOLD: u32 = 1;

/// Add a guardian, or reactivate a previously removed one.
///
/// Rejects: a non-owner caller, an uneditable vault, a duplicate active
/// guardian, an address equal to the owner or the contract, and a list already
/// at [`MAX_GUARDIANS`] slots.
pub fn add(env: &Env, vault_id: u64, guardian: Address) -> Result<(), HeirVaultError> {
    let mut vault = storage::load_vault(env, vault_id)?;
    vault::require_owner(&vault);
    vault::sync_status(env, &mut vault);
    vault::require_editable(&vault)?;

    vault::require_valid_party(env, &vault, &guardian)?;

    if vault
        .guardians
        .iter()
        .any(|slot| slot.active && slot.address == guardian)
    {
        return Err(HeirVaultError::DuplicateGuardian);
    }

    match slot_index(&vault, &guardian) {
        Some(index) => {
            let mut slot = vault
                .guardians
                .get(index)
                .ok_or(HeirVaultError::InvalidGuardian)?;
            slot.active = true;
            slot.approved = false;
            vault.guardians.set(index, slot);
        }
        None => {
            if vault.guardians.len() >= MAX_GUARDIANS {
                return Err(HeirVaultError::TooManyGuardians);
            }
            vault.guardians.push_back(Guardian {
                address: guardian.clone(),
                active: true,
                approved: false,
            });
        }
    }

    if vault.guardian_threshold == 0 {
        vault.guardian_threshold = DEFAULT_THRESHOLD;
    }

    storage::save_vault(env, &vault);
    events::guardian_added(
        env,
        vault_id,
        &guardian,
        vault.active_guardians(),
        vault.guardian_threshold,
    );
    Ok(())
}

/// Deactivate a guardian slot.
///
/// Refused if the removal would leave a guardian-gated vault unable to reach
/// its threshold. Dropping the last guardian resets the threshold to `0`, which
/// is only legal when the vault's activation mode does not consult guardians.
pub fn remove(env: &Env, vault_id: u64, guardian: Address) -> Result<(), HeirVaultError> {
    let mut vault = storage::load_vault(env, vault_id)?;
    vault::require_owner(&vault);
    vault::sync_status(env, &mut vault);
    vault::require_editable(&vault)?;

    let index = active_slot_index(&vault, &guardian).ok_or(HeirVaultError::InvalidGuardian)?;

    let mut slot = vault
        .guardians
        .get(index)
        .ok_or(HeirVaultError::InvalidGuardian)?;
    slot.active = false;
    slot.approved = false;
    vault.guardians.set(index, slot);

    let remaining = vault.active_guardians();
    let next_threshold = if remaining == 0 {
        0
    } else {
        vault.guardian_threshold
    };

    if vault.activation_mode.requires_guardian_approval()
        && (remaining == 0 || next_threshold == 0 || next_threshold > remaining)
    {
        return Err(HeirVaultError::InvalidThreshold);
    }

    vault.guardian_threshold = next_threshold;
    vault.guardian_approvals = recount_approvals(&vault);

    storage::save_vault(env, &vault);
    events::guardian_removed(
        env,
        vault_id,
        &guardian,
        remaining,
        vault.guardian_threshold,
    );
    Ok(())
}

/// Change how many guardian approvals a release needs.
///
/// Requires `1 <= threshold <= active guardians`, and requires at least one
/// guardian to remain when the vault's activation mode consults them.
pub fn set_threshold(env: &Env, vault_id: u64, threshold: u32) -> Result<(), HeirVaultError> {
    let mut vault = storage::load_vault(env, vault_id)?;
    vault::require_owner(&vault);
    vault::sync_status(env, &mut vault);
    vault::require_editable(&vault)?;

    let guardian_count = vault.active_guardians();
    if threshold == 0 || threshold > guardian_count {
        return Err(HeirVaultError::InvalidThreshold);
    }

    vault.guardian_threshold = threshold;
    // Lowering the bar can make previously-recorded approvals decisive, so the
    // cached count is recomputed from the slots rather than trusted.
    vault.guardian_approvals = recount_approvals(&vault);

    storage::save_vault(env, &vault);
    events::guardian_threshold_updated(env, vault_id, threshold, guardian_count);
    Ok(())
}

/// Record `guardian`'s approval of the current activation attempt.
///
/// The caller must authenticate as the guardian. Approving does **not** release
/// the vault by itself: once the threshold is met, anyone may call
/// `activate_vault`, which re-checks every activation condition. Keeping the two
/// steps separate means the approval path and the activation path have exactly
/// one place each to audit.
pub fn approve(env: &Env, vault_id: u64, guardian: Address) -> Result<(), HeirVaultError> {
    let mut vault = storage::load_vault(env, vault_id)?;
    guardian.require_auth();
    vault::sync_status(env, &mut vault);
    vault::require_live(&vault)?;

    if !vault.activation_mode.requires_guardian_approval() {
        return Err(HeirVaultError::ApprovalNotEnabled);
    }

    let index = active_slot_index(&vault, &guardian).ok_or(HeirVaultError::InvalidGuardian)?;

    let mut slot = vault
        .guardians
        .get(index)
        .ok_or(HeirVaultError::InvalidGuardian)?;
    if slot.approved {
        return Err(HeirVaultError::AlreadyApproved);
    }
    slot.approved = true;
    vault.guardians.set(index, slot);

    // Counted from the slots, never incremented blind: the threshold is a count
    // of distinct guardians by construction.
    vault.guardian_approvals = recount_approvals(&vault);

    storage::save_vault(env, &vault);
    events::guardian_approved(
        env,
        vault_id,
        &guardian,
        vault.guardian_approvals,
        vault.guardian_threshold,
    );
    Ok(())
}

/// Recompute the approval count from the guardian slots.
fn recount_approvals(vault: &Vault) -> u32 {
    let mut count = 0;
    for slot in vault.guardians.iter() {
        if slot.active && slot.approved {
            count += 1;
        }
    }
    count
}

/// Clear every approval flag and the counter.
///
/// Called by `check_in`: the owner proving they are alive invalidates the
/// activation attempt those votes belonged to.
pub fn reset_approvals(vault: &mut Vault) {
    for index in 0..vault.guardians.len() {
        if let Some(mut slot) = vault.guardians.get(index) {
            if slot.approved {
                slot.approved = false;
                vault.guardians.set(index, slot);
            }
        }
    }
    vault.guardian_approvals = 0;
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// Live approval position for a vault.
pub fn status(env: &Env, vault_id: u64) -> Result<GuardianStatus, HeirVaultError> {
    let vault = storage::load_vault(env, vault_id)?;
    let guardian_count = vault.active_guardians();
    let threshold = vault.guardian_threshold;
    Ok(GuardianStatus {
        approvals: vault.guardian_approvals,
        threshold,
        guardian_count,
        threshold_met: threshold > 0 && guardian_count > 0 && vault.guardian_approvals >= threshold,
    })
}

/// One page of guardian slots, including deactivated ones.
pub fn get_page(
    env: &Env,
    vault_id: u64,
    offset: u32,
    limit: u32,
) -> Result<GuardianPage, HeirVaultError> {
    let vault = storage::load_vault(env, vault_id)?;
    let limit = storage::clamp_limit(limit)?;
    let total = vault.guardians.len();

    let mut items: Vec<Guardian> = Vec::new(env);
    let mut cursor = offset;
    while cursor < total && items.len() < limit {
        if let Some(slot) = vault.guardians.get(cursor) {
            items.push_back(slot);
        }
        cursor += 1;
    }

    let meta = storage::page_meta(total, offset, limit, items.len());
    Ok(GuardianPage { items, meta })
}
