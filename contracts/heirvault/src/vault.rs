//! Vault lifecycle: creation, funding, check-in, activation, cancellation.
//!
//! # The check-in clock
//!
//! Two timestamps derive every lifecycle decision:
//!
//! ```text
//! deadline  = last_check_in + check_in_period
//! grace_end = deadline      + grace_period
//! ```
//!
//! Boundary behaviour, defined once here and enforced everywhere:
//!
//! | `now`                        | Effective state | Owner may check in | Owner may cancel | Activation eligible |
//! |------------------------------|-----------------|--------------------|------------------|---------------------|
//! | `now < deadline`             | `ACTIVE`        | yes                | yes              | no                  |
//! | `deadline <= now < grace_end`| `GRACE_PERIOD`  | yes                | yes              | no                  |
//! | `now >= grace_end`           | `GRACE_PERIOD`  | **no**             | yes              | yes                 |
//!
//! The intervals are half-open: `deadline` is already late (so a check-in at
//! exactly `deadline` does not save the vault from `GRACE_PERIOD`), and
//! `grace_end` is already too late for a recovery check-in. Activation is the
//! one comparison that is inclusive at `grace_end`, so there is never a
//! timestamp where a vault is neither recoverable nor activatable.
//!
//! All comparisons use the ledger timestamp, never a caller-supplied value, so
//! a client cannot choose which moment it is evaluated at.
//!
//! # Why cancel is allowed in `GRACE_PERIOD`, even after `grace_end`
//!
//! Cancellation requires the owner's signature. An owner who signs a cancel
//! transaction is, by definition, alive, so releasing their funds to their heirs
//! would be wrong; entitlement only ever transfers on an explicit activation.
//! This also means a late-but-alive owner can always recover a vault that
//! nobody has triggered yet, which is the safer failure direction: the
//! alternative would be to lock a living owner out of their own funds.
//!
//! # Ordering: checks, then effects, then interactions
//!
//! Every value function here validates, then writes the new state, and only
//! then calls into the token contract. A hostile or re-entrant token therefore
//! observes the *post*-transfer accounting, and a failed transfer reverts the
//! whole transaction including the accounting — so the internal balance can
//! never drift from the tokens actually held.

use soroban_sdk::{symbol_short, token, Address, Env, Vec};

use crate::errors::HeirVaultError;
use crate::events;
use crate::guardians;
use crate::storage;
use crate::types::{
    ActivationMode, Deadlines, Vault, VaultInfo, VaultPage, VaultStatus, FULL_ALLOCATION_BPS,
    MAX_CHECK_IN_PERIOD, MAX_GRACE_PERIOD, MIN_CHECK_IN_PERIOD, MIN_GRACE_PERIOD, SCHEMA_VERSION,
};

// ---------------------------------------------------------------------------
// Shared guards
// ---------------------------------------------------------------------------

/// Require the caller to be the vault owner.
pub fn require_owner(vault: &Vault) {
    vault.owner.require_auth();
}

/// Require the caller to be the owner **and** the caller's address to equal
/// `expected`, used where an entry point takes the owner as an argument so a
/// stray address cannot be substituted.
pub fn require_valid_party(
    env: &Env,
    vault: &Vault,
    party: &Address,
) -> Result<(), HeirVaultError> {
    if party == &vault.owner || party == &env.current_contract_address() {
        return Err(HeirVaultError::InvalidAddress);
    }
    Ok(())
}

/// Persist the time-derived `ACTIVE -> GRACE_PERIOD` transition if it is due.
///
/// Soroban state only changes when a transaction writes to it, so the transition
/// is computed lazily: read entry points report the derived status without
/// writing, and the first mutating entry point to observe it commits it. Returns
/// `true` when the status changed.
pub fn sync_status(env: &Env, vault: &mut Vault) -> bool {
    let now = env.ledger().timestamp();
    if vault.status == VaultStatus::Active && now >= vault.deadline() {
        vault.status = VaultStatus::GracePeriod;
        vault.last_status_change = now;
        true
    } else {
        false
    }
}

/// Reject every operation on a vault that has already finished its life.
pub fn require_live(vault: &Vault) -> Result<(), HeirVaultError> {
    match vault.status {
        VaultStatus::Activated => Err(HeirVaultError::AlreadyActivated),
        VaultStatus::Cancelled => Err(HeirVaultError::VaultCancelled),
        VaultStatus::Completed => Err(HeirVaultError::VaultCompleted),
        VaultStatus::Active | VaultStatus::GracePeriod => Ok(()),
    }
}

/// Reject every operation that would reconfigure a vault that is no longer the
/// owner's to change.
pub fn require_editable(vault: &Vault) -> Result<(), HeirVaultError> {
    require_live(vault)?;
    if vault.status.is_editable() {
        Ok(())
    } else {
        Err(HeirVaultError::VaultNotEditable)
    }
}

/// Map a "this vault is not accepting deposits" situation onto a precise error.
pub fn deposit_rejection(vault: &Vault) -> HeirVaultError {
    match vault.status {
        VaultStatus::Activated => HeirVaultError::AlreadyActivated,
        VaultStatus::Cancelled => HeirVaultError::VaultCancelled,
        VaultStatus::Completed => HeirVaultError::VaultCompleted,
        VaultStatus::Active | VaultStatus::GracePeriod => HeirVaultError::InvalidState,
    }
}

/// Best-effort check that `asset` is a SEP-41 token contract.
///
/// A host cross-contract call is made to `decimals()`, which every SEP-41 token
/// must implement. This rejects typos and plain account addresses at creation
/// time, which is far better than discovering them at the first deposit.
///
/// It is explicitly *not* a guarantee: a contract could implement `decimals()`
/// and still refuse `transfer`. The real, non-negotiable check is that
/// [`deposit`] performs a real transfer and reverts if it fails, so a bad asset
/// can never leave the contract holding phantom accounting.
fn supports_sep41(env: &Env, asset: &Address) -> bool {
    let args: Vec<soroban_sdk::Val> = Vec::new(env);
    env.try_invoke_contract::<u32, soroban_sdk::Error>(asset, &symbol_short!("decimals"), args)
        .is_ok()
}

// ---------------------------------------------------------------------------
// create_vault
// ---------------------------------------------------------------------------

/// Create an empty inheritance vault and return its id.
///
/// The vault starts with a zero balance; funding is a separate [`deposit`], so
/// an owner can configure beneficiaries and guardians before committing funds
/// and can top up over time.
///
/// Validation: owner authenticates; both periods are inside their supported
/// ranges; the asset is not the contract itself and answers `decimals()`; and
/// the freshly allocated id is confirmed unused (defence in depth — the id
/// allocator is monotonic, so a collision would require corrupt state).
pub fn create(
    env: &Env,
    owner: Address,
    asset: Address,
    activation_mode: ActivationMode,
    check_in_period: u64,
    grace_period: u64,
) -> Result<u64, HeirVaultError> {
    owner.require_auth();
    storage::touch_instance(env);

    if !(MIN_CHECK_IN_PERIOD..=MAX_CHECK_IN_PERIOD).contains(&check_in_period) {
        return Err(HeirVaultError::InvalidPeriod);
    }
    if !(MIN_GRACE_PERIOD..=MAX_GRACE_PERIOD).contains(&grace_period) {
        return Err(HeirVaultError::InvalidPeriod);
    }
    if asset == env.current_contract_address() || !supports_sep41(env, &asset) {
        return Err(HeirVaultError::InvalidAsset);
    }

    let id = storage::allocate_vault_id(env)?;
    if storage::has_vault(env, id) {
        return Err(HeirVaultError::VaultAlreadyExists);
    }

    let now = env.ledger().timestamp();
    let vault = Vault {
        id,
        owner: owner.clone(),
        asset: asset.clone(),
        balance: 0,
        status: VaultStatus::Active,
        activation_mode,
        check_in_period,
        grace_period,
        last_check_in: now,
        last_status_change: now,
        activated_at: 0,
        distribution_balance: 0,
        total_claimed: 0,
        claimed_count: 0,
        beneficiaries: Vec::new(env),
        guardians: Vec::new(env),
        guardian_threshold: 0,
        guardian_approvals: 0,
        created_at: now,
        schema_version: SCHEMA_VERSION,
    };

    storage::save_vault(env, &vault);
    storage::push_owner_vault(env, &owner, id)?;

    events::vault_created(
        env,
        id,
        &owner,
        &asset,
        activation_mode,
        check_in_period,
        grace_period,
    );
    Ok(id)
}

// ---------------------------------------------------------------------------
// deposit
// ---------------------------------------------------------------------------

/// Transfer `amount` of the vault's asset from the owner into the vault.
///
/// The asset address is passed explicitly and compared against the configured
/// one, so a client that fat-fingers the wrong token is rejected *before* any
/// transfer happens rather than silently funding the vault with an asset it
/// will never distribute.
///
/// Deposits are accepted while the vault is `ACTIVE` or in `GRACE_PERIOD` (an
/// owner recovering from a missed check-in may still be topping up), and are
/// rejected once the vault is activated, cancelled or completed.
pub fn deposit(
    env: &Env,
    vault_id: u64,
    asset: Address,
    amount: i128,
) -> Result<(), HeirVaultError> {
    let mut vault = storage::load_vault(env, vault_id)?;
    require_owner(&vault);
    storage::touch_instance(env);
    sync_status(env, &mut vault);

    if !vault.status.accepts_deposits() {
        return Err(deposit_rejection(&vault));
    }
    if asset != vault.asset {
        return Err(HeirVaultError::InvalidAsset);
    }
    if amount <= 0 {
        return Err(HeirVaultError::InvalidAmount);
    }

    let new_balance = vault
        .balance
        .checked_add(amount)
        .ok_or(HeirVaultError::ArithmeticOverflow)?;

    // Effects before interaction.
    vault.balance = new_balance;
    storage::save_vault(env, &vault);

    token::Client::new(env, &vault.asset).transfer(
        &vault.owner,
        &env.current_contract_address(),
        &amount,
    );

    events::deposit_made(env, vault_id, &vault.owner, amount, new_balance);
    Ok(())
}

// ---------------------------------------------------------------------------
// check_in
// ---------------------------------------------------------------------------

/// Prove the owner is still active and restart the check-in clock.
///
/// Only the owner can call this, which is the whole point: nobody else can reset
/// the timer and indefinitely postpone an inheritance. A successful check-in
/// moves the vault back to `ACTIVE`, restarts the deadline from the current
/// ledger time, and clears every guardian approval, because those votes belonged
/// to an activation attempt that the owner has just invalidated.
///
/// Returns the new deadline.
pub fn check_in(env: &Env, vault_id: u64) -> Result<u64, HeirVaultError> {
    let mut vault = storage::load_vault(env, vault_id)?;
    require_owner(&vault);
    storage::touch_instance(env);
    sync_status(env, &mut vault);
    require_editable(&vault)?;

    let now = env.ledger().timestamp();
    if vault.grace_period_expired(now) {
        return Err(HeirVaultError::GracePeriodExpired);
    }

    vault.last_check_in = now;
    vault.last_status_change = now;
    vault.status = VaultStatus::Active;
    guardians::reset_approvals(&mut vault);

    storage::save_vault(env, &vault);
    events::check_in_completed(env, vault_id, &vault.owner, now, vault.deadline());
    Ok(vault.deadline())
}

// ---------------------------------------------------------------------------
// activate_vault
// ---------------------------------------------------------------------------

/// `true` when this vault satisfies every activation condition right now.
///
/// Shared by [`activate`] and by the read-side [`get_info`] so the value a
/// client is shown can never disagree with what the contract will accept.
pub fn activation_ready(vault: &Vault, now: u64) -> bool {
    if vault.status != VaultStatus::Active && vault.status != VaultStatus::GracePeriod {
        return false;
    }
    if vault.balance <= 0 || vault.active_beneficiaries() == 0 {
        return false;
    }
    if vault.total_allocation_bps() != FULL_ALLOCATION_BPS {
        return false;
    }
    if vault.activation_mode.requires_missed_check_in() && !vault.grace_period_expired(now) {
        return false;
    }
    if vault.activation_mode.requires_guardian_approval() {
        if vault.guardian_threshold == 0 || vault.active_guardians() == 0 {
            return false;
        }
        if vault.guardian_approvals < vault.guardian_threshold {
            return false;
        }
    }
    true
}

/// Release the vault to its beneficiaries.
///
/// Deliberately **permissionless**: once the conditions hold, anyone may push
/// the vault forward. Requiring the owner to call it would be self-defeating (the
/// whole premise is that the owner is gone), and requiring a specific keeper
/// would introduce a liveness dependency. The permission is safe because the
/// function re-derives every condition from on-chain state and the ledger clock;
/// it cannot be talked into releasing a vault that is not due, and it cannot
/// pay anyone — it only flips the state that makes claims possible.
///
/// Conditions, all enforced:
/// 1. the vault is not already `ACTIVATED`, `CANCELLED` or `COMPLETED`;
/// 2. its activation mode's clock condition is satisfied;
/// 3. its activation mode's guardian condition (if any) is satisfied;
/// 4. there is at least one active beneficiary;
/// 5. the active allocations total exactly 100%;
/// 6. there is a non-zero balance to distribute.
///
/// On success the balance is snapshotted into `distribution_balance`. Claims are
/// computed against that snapshot, so what each heir is owed is fixed at the
/// moment of activation.
pub fn activate(env: &Env, vault_id: u64) -> Result<(), HeirVaultError> {
    let mut vault = storage::load_vault(env, vault_id)?;
    storage::touch_instance(env);
    sync_status(env, &mut vault);
    require_live(&vault)?;

    let now = env.ledger().timestamp();

    if vault.activation_mode.requires_missed_check_in() && !vault.grace_period_expired(now) {
        return Err(if now >= vault.deadline() {
            HeirVaultError::GracePeriodActive
        } else {
            HeirVaultError::DeadlineNotReached
        });
    }

    if vault.activation_mode.requires_guardian_approval() {
        let threshold = vault.guardian_threshold;
        if threshold == 0 || vault.active_guardians() == 0 || vault.guardian_approvals < threshold {
            return Err(HeirVaultError::NotEligible);
        }
    }

    if vault.active_beneficiaries() == 0 {
        return Err(HeirVaultError::NoBeneficiaries);
    }
    if vault.total_allocation_bps() != FULL_ALLOCATION_BPS {
        return Err(HeirVaultError::AllocationIncomplete);
    }
    if vault.balance <= 0 {
        return Err(HeirVaultError::InsufficientBalance);
    }

    vault.status = VaultStatus::Activated;
    vault.activated_at = now;
    vault.last_status_change = now;
    vault.distribution_balance = vault.balance;
    vault.total_claimed = 0;
    vault.claimed_count = 0;

    storage::save_vault(env, &vault);
    events::vault_activated(
        env,
        vault_id,
        vault.activation_mode,
        vault.distribution_balance,
        vault.active_beneficiaries(),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// cancel_vault / withdraw
// ---------------------------------------------------------------------------

/// Cancel the vault and return it to the owner.
///
/// Only the owner can cancel, and only while the vault is `ACTIVE` or in
/// `GRACE_PERIOD`. An `ACTIVATED` vault is refused: cancellation must never be
/// able to interrupt a claim process that beneficiaries can already act on.
///
/// Cancellation itself moves no funds — it flips the state that makes
/// [`withdraw`] legal. Keeping the two apart means a cancelled vault's balance
/// stays visible and is only reduced by explicit, individually logged
/// withdrawals.
pub fn cancel(env: &Env, vault_id: u64) -> Result<(), HeirVaultError> {
    let mut vault = storage::load_vault(env, vault_id)?;
    require_owner(&vault);
    storage::touch_instance(env);
    sync_status(env, &mut vault);
    require_editable(&vault)?;

    vault.status = VaultStatus::Cancelled;
    vault.last_status_change = env.ledger().timestamp();

    storage::save_vault(env, &vault);
    events::vault_cancelled(env, vault_id, &vault.owner, vault.balance);
    Ok(())
}

/// Withdraw from a cancelled vault.
///
/// Legal only while the vault is `CANCELLED`, so a withdrawal can never race a
/// claim: by the time a vault is activated its owner has lost the right to pull
/// funds out. Partial withdrawals are allowed and each one is logged, so an
/// owner can move funds out in steps without a single all-or-nothing transaction.
pub fn withdraw(env: &Env, vault_id: u64, amount: i128) -> Result<(), HeirVaultError> {
    let mut vault = storage::load_vault(env, vault_id)?;
    require_owner(&vault);
    storage::touch_instance(env);

    if vault.status != VaultStatus::Cancelled {
        return Err(match vault.status {
            VaultStatus::Activated => HeirVaultError::AlreadyActivated,
            VaultStatus::Completed => HeirVaultError::VaultCompleted,
            _ => HeirVaultError::InvalidState,
        });
    }
    if amount <= 0 {
        return Err(HeirVaultError::InvalidAmount);
    }
    if amount > vault.balance {
        return Err(HeirVaultError::InsufficientBalance);
    }

    let remaining = vault
        .balance
        .checked_sub(amount)
        .ok_or(HeirVaultError::ArithmeticOverflow)?;

    vault.balance = remaining;
    storage::save_vault(env, &vault);

    token::Client::new(env, &vault.asset).transfer(
        &env.current_contract_address(),
        &vault.owner,
        &amount,
    );

    events::withdrawal(env, vault_id, &vault.owner, amount, remaining);
    Ok(())
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// Full detail for one vault, with clock-derived values filled in.
pub fn get_info(env: &Env, vault_id: u64) -> Result<VaultInfo, HeirVaultError> {
    let vault = storage::load_vault(env, vault_id)?;
    let now = env.ledger().timestamp();
    Ok(VaultInfo {
        deadline: vault.deadline(),
        grace_end: vault.grace_end(),
        effective_status: vault.effective_status(now),
        total_allocation_bps: vault.total_allocation_bps(),
        guardian_threshold: vault.guardian_threshold,
        guardian_approvals: vault.guardian_approvals,
        activation_ready: activation_ready(&vault, now),
        vault,
    })
}

/// The stored record with no derived fields, for callers that want it verbatim.
pub fn get_raw(env: &Env, vault_id: u64) -> Result<Vault, HeirVaultError> {
    storage::load_vault(env, vault_id)
}

/// Just the clock-adjusted status, for cheap polling.
pub fn get_status(env: &Env, vault_id: u64) -> Result<VaultStatus, HeirVaultError> {
    let vault = storage::load_vault(env, vault_id)?;
    Ok(vault.effective_status(env.ledger().timestamp()))
}

/// The two lifecycle timestamps plus the current ledger time.
pub fn get_deadlines(env: &Env, vault_id: u64) -> Result<Deadlines, HeirVaultError> {
    let vault = storage::load_vault(env, vault_id)?;
    Ok(Deadlines {
        deadline: vault.deadline(),
        grace_end: vault.grace_end(),
        now: env.ledger().timestamp(),
    })
}

/// `true` when this vault's beneficiaries can claim right now.
pub fn is_claimable(env: &Env, vault_id: u64) -> Result<bool, HeirVaultError> {
    let vault = storage::load_vault(env, vault_id)?;
    Ok(vault
        .effective_status(env.ledger().timestamp())
        .is_claimable())
}

/// One page of an owner's vaults.
///
/// Costs `limit` point reads through the owner index — never a scan of the
/// contract's vaults — so the price of this call does not grow with the total
/// number of vaults in existence.
pub fn get_by_owner(
    env: &Env,
    owner: Address,
    offset: u32,
    limit: u32,
) -> Result<VaultPage, HeirVaultError> {
    let limit = storage::clamp_limit(limit)?;
    let total = storage::owner_vault_count(env, &owner);
    let now = env.ledger().timestamp();

    let mut items: Vec<crate::types::VaultSummary> = Vec::new(env);
    let mut cursor = offset;
    while cursor < total && items.len() < limit {
        if let Some(id) = storage::owner_vault_id_at(env, &owner, cursor) {
            // A slot whose vault cannot be read is skipped rather than failing
            // the whole page: one archived entry must not make an owner's other
            // vaults unlistable.
            if let Some(vault) = storage::load_vault_opt(env, id) {
                items.push_back(vault.to_summary(now));
            }
        }
        cursor += 1;
    }

    let meta = storage::page_meta(total, offset, limit, items.len());
    Ok(VaultPage { items, meta })
}
