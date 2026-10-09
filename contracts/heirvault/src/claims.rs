//! Inheritance claims.
//!
//! # What an heir is owed
//!
//! Activation snapshots the vault's balance into
//! [`Vault::distribution_balance`]. Every claim after that is computed from the
//! **snapshot**, never from the live balance, so one heir's payout can never
//! change another's and a hostile caller cannot manipulate the timing of a
//! claim to shift value between heirs.
//!
//! ```text
//! entitlement(heir) = floor(distribution_balance * allocation_bps / 10_000)
//! ```
//!
//! Integer division loses the remainder, which is the "dust" that would
//! otherwise stay locked forever. HeirVault gives it to **the last heir to
//! claim**, who receives `balance` — that is, exactly
//! `distribution_balance - total_claimed` — instead of its floored share. The
//! invariant this buys is strong and easy to test:
//!
//! > `sum(all payouts) == distribution_balance`, and the vault's balance reaches
//! > exactly zero when the last heir claims.
//!
//! # Why claims are pulled, not pushed
//!
//! `activate_vault` does not transfer anything. Each heir claims for itself.
//! A push-based design has to pay every heir inside one transaction, which caps
//! the design at the number of transfers that fit in a single transaction's
//! budget — and any heir whose transfer fails (a frozen account, a contract that
//! rejects the transfer) would block every other heir. Pulled claims make each
//! heir's payout independent: one heir's problem cannot strand another's funds.
//!
//! # Double-claim safety
//!
//! Claims are guarded by a `claimed` flag on the heir's own slot, set before the
//! transfer. There is no path that pays an heir twice: the flag is checked
//! first, the state is written before the interaction, and a reverted transfer
//! reverts the flag with it, so a failed claim leaves the heir able to try again
//! but never able to be paid twice.

use soroban_sdk::{token, Address, Env, Vec};

use crate::errors::HeirVaultError;
use crate::events;
use crate::storage;
use crate::types::{ClaimPage, ClaimRecord, Vault, VaultStatus, BPS_DENOMINATOR};

/// `balance * bps / 10_000`, with checked arithmetic.
pub fn share_of(balance: i128, allocation_bps: u32) -> Result<i128, HeirVaultError> {
    balance
        .checked_mul(allocation_bps as i128)
        .ok_or(HeirVaultError::ArithmeticOverflow)?
        .checked_div(BPS_DENOMINATOR as i128)
        .ok_or(HeirVaultError::ArithmeticOverflow)
}

/// The un-rounded share of the distribution balance for slot `index`.
pub fn entitlement(vault: &Vault, index: u32) -> i128 {
    match vault.beneficiaries.get(index) {
        Some(slot) if slot.active => {
            share_of(vault.distribution_balance, slot.allocation_bps).unwrap_or(0)
        }
        _ => 0,
    }
}

/// Claim the calling beneficiary's allocation.
///
/// Returns the amount transferred (which is `0` only in the dust case where an
/// heir's share rounds below one unit; the slot is still marked claimed so the
/// vault can reach `COMPLETED`).
///
/// Rejected when: the vault is not activated; the caller is not an active
/// beneficiary; the caller already claimed; or the payout would exceed the
/// vault's remaining balance.
pub fn claim(env: &Env, vault_id: u64, beneficiary: Address) -> Result<i128, HeirVaultError> {
    let mut vault = storage::load_vault(env, vault_id)?;
    beneficiary.require_auth();
    storage::touch_instance(env);

    if vault.status != VaultStatus::Activated {
        return Err(match vault.status {
            VaultStatus::Completed => HeirVaultError::VaultCompleted,
            VaultStatus::Cancelled => HeirVaultError::VaultCancelled,
            _ => HeirVaultError::NotActivated,
        });
    }

    let index = vault
        .beneficiary_index(&beneficiary)
        .ok_or(HeirVaultError::InvalidBeneficiary)?;
    let mut slot = vault
        .beneficiaries
        .get(index)
        .ok_or(HeirVaultError::InvalidBeneficiary)?;
    if slot.claimed {
        return Err(HeirVaultError::AlreadyClaimed);
    }

    // The last heir still outstanding absorbs the rounding remainder, which is
    // exactly the vault's remaining balance.
    let mut others_outstanding = 0u32;
    for (other_index, other) in vault.beneficiaries.iter().enumerate() {
        if other_index as u32 != index && other.active && !other.claimed {
            others_outstanding += 1;
        }
    }

    let amount = if others_outstanding == 0 {
        vault.balance
    } else {
        share_of(vault.distribution_balance, slot.allocation_bps)?
    };

    if amount < 0 || amount > vault.balance {
        return Err(HeirVaultError::InsufficientBalance);
    }

    // Effects before interaction.
    slot.claimed = true;
    slot.claimed_amount = amount;
    vault.beneficiaries.set(index, slot);

    vault.total_claimed = vault
        .total_claimed
        .checked_add(amount)
        .ok_or(HeirVaultError::ArithmeticOverflow)?;
    vault.claimed_count += 1;
    vault.balance = vault
        .balance
        .checked_sub(amount)
        .ok_or(HeirVaultError::ArithmeticOverflow)?;
    vault.last_status_change = env.ledger().timestamp();

    let completed = vault.claimed_count >= vault.active_beneficiaries();
    if completed {
        vault.status = VaultStatus::Completed;
    }

    storage::save_vault(env, &vault);

    // A dust payout of zero needs no transfer; every other payout is a real
    // SEP-41 transfer, so the vault can never "pay" an heir with accounting
    // alone.
    if amount > 0 {
        token::Client::new(env, &vault.asset).transfer(
            &env.current_contract_address(),
            &beneficiary,
            &amount,
        );
    }

    events::inheritance_claimed(
        env,
        vault_id,
        &beneficiary,
        amount,
        vault.total_claimed,
        vault.balance,
    );
    if completed {
        events::vault_completed(env, vault_id, vault.total_claimed, vault.claimed_count);
    }

    Ok(amount)
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// One page of claim positions, including heirs that have not claimed yet.
pub fn get_page(
    env: &Env,
    vault_id: u64,
    offset: u32,
    limit: u32,
) -> Result<ClaimPage, HeirVaultError> {
    let vault = storage::load_vault(env, vault_id)?;
    let limit = storage::clamp_limit(limit)?;
    let total = vault.beneficiaries.len();

    let mut items: Vec<ClaimRecord> = Vec::new(env);
    let mut cursor = offset;
    while cursor < total && items.len() < limit {
        if let Some(slot) = vault.beneficiaries.get(cursor) {
            items.push_back(ClaimRecord {
                beneficiary: slot.address.clone(),
                allocation_bps: slot.allocation_bps,
                entitlement: if slot.active {
                    entitlement(&vault, cursor)
                } else {
                    0
                },
                claimed_amount: slot.claimed_amount,
                claimed: slot.claimed,
            });
        }
        cursor += 1;
    }

    let meta = storage::page_meta(total, offset, limit, items.len());
    Ok(ClaimPage { items, meta })
}

/// The claim position for one beneficiary.
pub fn get_for(
    env: &Env,
    vault_id: u64,
    beneficiary: Address,
) -> Result<ClaimRecord, HeirVaultError> {
    let vault = storage::load_vault(env, vault_id)?;
    let index = vault
        .beneficiary_index(&beneficiary)
        .ok_or(HeirVaultError::InvalidBeneficiary)?;
    let slot = vault
        .beneficiaries
        .get(index)
        .ok_or(HeirVaultError::InvalidBeneficiary)?;
    Ok(ClaimRecord {
        beneficiary: slot.address.clone(),
        allocation_bps: slot.allocation_bps,
        entitlement: entitlement(&vault, index),
        claimed_amount: slot.claimed_amount,
        claimed: slot.claimed,
    })
}

/// Amount still undistributed on an activated vault.
///
/// `0` before activation (nothing is distributable yet), and
/// `distribution_balance - total_claimed` afterwards.
pub fn remaining(env: &Env, vault_id: u64) -> Result<i128, HeirVaultError> {
    let vault = storage::load_vault(env, vault_id)?;
    if vault.status != VaultStatus::Activated && vault.status != VaultStatus::Completed {
        return Ok(0);
    }
    Ok(vault.balance)
}
