#![no_std]
//! # HeirVault
//!
//! A programmable digital inheritance vault for Stellar/Soroban.
//!
//! An owner locks supported SEP-41 assets into a vault, names beneficiaries with
//! percentage shares, and proves they are still active by checking in on a
//! schedule. If they stop checking in, and the grace period passes, the vault is
//! released and each beneficiary can claim its share — without a lawyer, a court
//! or a custodian.
//!
//! ## Module map
//!
//! | Module           | Responsibility                                                   |
//! |------------------|------------------------------------------------------------------|
//! | [`errors`]       | Stable numeric error ABI                                          |
//! | [`types`]        | Stored and returned value types, lifecycle enums, constants        |
//! | [`storage`]      | Ledger key layout, TTL management, schema versioning, paging       |
//! | [`events`]       | Typed event payloads for every state change                        |
//! | [`vault`]        | Creation, deposits, check-in, activation, cancellation, withdrawal |
//! | [`beneficiaries`]| Beneficiary slots and the allocation invariant                     |
//! | [`guardians`]    | Guardian slots, M-of-N approval, anti-lockout rules                |
//! | [`claims`]       | Per-heir claims, rounding, double-claim prevention                 |
//!
//! The `#[contractimpl]` block below is intentionally thin: it validates the ABI
//! shape and delegates. All logic lives in the modules, where it can be unit
//! tested without going through the contract boundary.
//!
//! ## Entry-point summary
//!
//! | Group          | Entry points |
//! |----------------|--------------|
//! | Setup          | `create_vault`, `deposit` |
//! | Beneficiaries  | `add_beneficiary`, `remove_beneficiary`, `update_beneficiary` |
//! | Guardians      | `add_guardian`, `remove_guardian`, `set_guardian_threshold`, `guardian_approve` |
//! | Lifecycle      | `check_in`, `activate_vault`, `cancel_vault`, `withdraw` |
//! | Claims         | `claim` |
//! | Reads          | `get_vault`, `get_vault_status`, `get_deadlines`, `is_claimable`,<br>`get_vaults_by_owner`, `get_vault_count`, `schema_version`,<br>`get_beneficiaries`, `get_active_beneficiaries`, `get_total_allocation`,<br>`get_guardians`, `get_guardian_status`, `get_claims`, `get_claim`,<br>`get_undistributed_balance` |

pub mod beneficiaries;
pub mod claims;
pub mod errors;
pub mod events;
pub mod guardians;
pub mod storage;
pub mod types;
pub mod vault;

#[cfg(test)]
mod profile;
#[cfg(test)]
mod test;

use soroban_sdk::{contract, contractimpl, contractmeta, Address, Env, Vec};

pub use crate::errors::HeirVaultError;
pub use crate::types::{
    ActivationMode, Beneficiary, BeneficiaryPage, ClaimPage, ClaimRecord, Deadlines, Guardian,
    GuardianPage, GuardianStatus, PageMeta, Vault, VaultInfo, VaultPage, VaultStatus, VaultSummary,
    BPS_DENOMINATOR, FULL_ALLOCATION_BPS, MAX_BENEFICIARIES, MAX_CHECK_IN_PERIOD, MAX_GRACE_PERIOD,
    MAX_GUARDIANS, MAX_PAGE_LIMIT, MIN_CHECK_IN_PERIOD, MIN_GRACE_PERIOD, SCHEMA_VERSION,
};

contractmeta!(key = "name", val = "HeirVault");
contractmeta!(key = "version", val = "0.1.0");
contractmeta!(
    key = "description",
    val = "Programmable digital inheritance vault: check-in schedules, beneficiary allocations, M-of-N guardians and pulled claims."
);

#[contract]
pub struct HeirVaultContract;

#[contractimpl]
impl HeirVaultContract {
    // -----------------------------------------------------------------
    // Setup
    // -----------------------------------------------------------------

    /// Create an empty inheritance vault and return its id.
    ///
    /// The vault starts unfunded; call [`HeirVaultContract::deposit`] to add
    /// assets. `activation_mode` decides which conditions release it later — see
    /// [`ActivationMode`].
    ///
    /// * `owner` — must authenticate; the only address that can configure,
    ///   check in, cancel or withdraw.
    /// * `asset` — a SEP-41 token contract. Probe-checked with `decimals()`.
    /// * `check_in_period` — seconds allowed between check-ins.
    /// * `grace_period` — recovery window after the check-in deadline.
    pub fn create_vault(
        env: Env,
        owner: Address,
        asset: Address,
        activation_mode: ActivationMode,
        check_in_period: u64,
        grace_period: u64,
    ) -> Result<u64, HeirVaultError> {
        vault::create(
            &env,
            owner,
            asset,
            activation_mode,
            check_in_period,
            grace_period,
        )
    }

    /// Deposit `amount` of the vault's asset from the owner into the vault.
    ///
    /// `asset` must equal the vault's configured asset, which rejects a wrong
    /// token before any transfer is attempted. Allowed while the vault is
    /// `ACTIVE` or in `GRACE_PERIOD`.
    pub fn deposit(
        env: Env,
        vault_id: u64,
        asset: Address,
        amount: i128,
    ) -> Result<(), HeirVaultError> {
        vault::deposit(&env, vault_id, asset, amount)
    }

    // -----------------------------------------------------------------
    // Beneficiaries
    // -----------------------------------------------------------------

    /// Add a beneficiary with `allocation_bps` basis points (10_000 == 100%).
    ///
    /// Only the owner, only while the vault is editable. Rejected when the share
    /// is zero or would push the total above 100%, when the address is already
    /// an active beneficiary, or when the vault is at [`MAX_BENEFICIARIES`] slots.
    pub fn add_beneficiary(
        env: Env,
        vault_id: u64,
        beneficiary: Address,
        allocation_bps: u32,
    ) -> Result<(), HeirVaultError> {
        beneficiaries::add(&env, vault_id, beneficiary, allocation_bps)
    }

    /// Deactivate a beneficiary and release its allocation.
    pub fn remove_beneficiary(
        env: Env,
        vault_id: u64,
        beneficiary: Address,
    ) -> Result<(), HeirVaultError> {
        beneficiaries::remove(&env, vault_id, beneficiary)
    }

    /// Change an active beneficiary's allocation.
    pub fn update_beneficiary(
        env: Env,
        vault_id: u64,
        beneficiary: Address,
        new_allocation_bps: u32,
    ) -> Result<(), HeirVaultError> {
        beneficiaries::update(&env, vault_id, beneficiary, new_allocation_bps)
    }

    /// One page of beneficiary slots, including deactivated ones.
    pub fn get_beneficiaries(
        env: Env,
        vault_id: u64,
        offset: u32,
        limit: u32,
    ) -> Result<BeneficiaryPage, HeirVaultError> {
        beneficiaries::get_page(&env, vault_id, offset, limit)
    }

    /// Every active beneficiary, unpaged. Bounded by [`MAX_BENEFICIARIES`].
    pub fn get_active_beneficiaries(
        env: Env,
        vault_id: u64,
    ) -> Result<Vec<Beneficiary>, HeirVaultError> {
        beneficiaries::get_active(&env, vault_id)
    }

    /// Sum of active allocations, in basis points.
    pub fn get_total_allocation(env: Env, vault_id: u64) -> Result<u32, HeirVaultError> {
        beneficiaries::total_allocation(&env, vault_id)
    }

    // -----------------------------------------------------------------
    // Guardians
    // -----------------------------------------------------------------

    /// Add a guardian. Threshold defaults to `1` on the first guardian added.
    pub fn add_guardian(env: Env, vault_id: u64, guardian: Address) -> Result<(), HeirVaultError> {
        guardians::add(&env, vault_id, guardian)
    }

    /// Remove a guardian.
    ///
    /// Refused when it would leave a guardian-gated vault unable to reach its
    /// threshold, which is what stops an owner from stranding a vault's funds.
    pub fn remove_guardian(
        env: Env,
        vault_id: u64,
        guardian: Address,
    ) -> Result<(), HeirVaultError> {
        guardians::remove(&env, vault_id, guardian)
    }

    /// Set how many guardian approvals a release requires. Must satisfy
    /// `1 <= threshold <= active guardians`.
    pub fn set_guardian_threshold(
        env: Env,
        vault_id: u64,
        threshold: u32,
    ) -> Result<(), HeirVaultError> {
        guardians::set_threshold(&env, vault_id, threshold)
    }

    /// Record the calling guardian's approval of the current activation attempt.
    ///
    /// `guardian` must authenticate. Each guardian may approve once per attempt;
    /// an owner check-in clears all approvals. Approving does not release the
    /// vault — call [`HeirVaultContract::activate_vault`] once the threshold is met.
    pub fn guardian_approve(
        env: Env,
        vault_id: u64,
        guardian: Address,
    ) -> Result<(), HeirVaultError> {
        guardians::approve(&env, vault_id, guardian)
    }

    /// One page of guardian slots, including deactivated ones.
    pub fn get_guardians(
        env: Env,
        vault_id: u64,
        offset: u32,
        limit: u32,
    ) -> Result<GuardianPage, HeirVaultError> {
        guardians::get_page(&env, vault_id, offset, limit)
    }

    /// Approvals recorded, threshold required, and whether the threshold is met.
    pub fn get_guardian_status(env: Env, vault_id: u64) -> Result<GuardianStatus, HeirVaultError> {
        guardians::status(&env, vault_id)
    }

    // -----------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------

    /// Prove the owner is still active and restart the check-in clock.
    ///
    /// Only the owner. Clears all guardian approvals. Returns the new deadline.
    /// Rejected once the grace period has expired.
    pub fn check_in(env: Env, vault_id: u64) -> Result<u64, HeirVaultError> {
        vault::check_in(&env, vault_id)
    }

    /// Release the vault to its beneficiaries. **Permissionless.**
    ///
    /// Succeeds only when the vault's activation mode is satisfied, at least one
    /// active beneficiary exists, allocations total exactly 100%, and there is a
    /// balance to distribute. Transfers nothing itself: it snapshots the balance
    /// and opens the claim phase.
    pub fn activate_vault(env: Env, vault_id: u64) -> Result<(), HeirVaultError> {
        vault::activate(&env, vault_id)
    }

    /// Cancel the vault. Only the owner, and never once the vault is activated.
    ///
    /// Cancellation moves no funds; it enables [`HeirVaultContract::withdraw`].
    pub fn cancel_vault(env: Env, vault_id: u64) -> Result<(), HeirVaultError> {
        vault::cancel(&env, vault_id)
    }

    /// Withdraw `amount` from a cancelled vault back to the owner.
    pub fn withdraw(env: Env, vault_id: u64, amount: i128) -> Result<(), HeirVaultError> {
        vault::withdraw(&env, vault_id, amount)
    }

    // -----------------------------------------------------------------
    // Claims
    // -----------------------------------------------------------------

    /// Claim the calling beneficiary's allocation from an activated vault.
    ///
    /// `beneficiary` must authenticate. Returns the amount transferred. The last
    /// heir to claim receives the rounding remainder, so a fully claimed vault's
    /// balance reaches exactly zero.
    pub fn claim(env: Env, vault_id: u64, beneficiary: Address) -> Result<i128, HeirVaultError> {
        claims::claim(&env, vault_id, beneficiary)
    }

    /// One page of claim positions, including heirs that have not claimed.
    pub fn get_claims(
        env: Env,
        vault_id: u64,
        offset: u32,
        limit: u32,
    ) -> Result<ClaimPage, HeirVaultError> {
        claims::get_page(&env, vault_id, offset, limit)
    }

    /// The claim position of one beneficiary.
    pub fn get_claim(
        env: Env,
        vault_id: u64,
        beneficiary: Address,
    ) -> Result<ClaimRecord, HeirVaultError> {
        claims::get_for(&env, vault_id, beneficiary)
    }

    /// Amount on an activated vault that no heir has claimed yet.
    pub fn get_undistributed_balance(env: Env, vault_id: u64) -> Result<i128, HeirVaultError> {
        claims::remaining(&env, vault_id)
    }

    // -----------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------

    /// Full vault detail plus deadline, grace end, derived status and readiness.
    pub fn get_vault(env: Env, vault_id: u64) -> Result<VaultInfo, HeirVaultError> {
        vault::get_info(&env, vault_id)
    }

    /// The clock-adjusted lifecycle status.
    pub fn get_vault_status(env: Env, vault_id: u64) -> Result<VaultStatus, HeirVaultError> {
        vault::get_status(&env, vault_id)
    }

    /// `deadline`, `grace_end` and the current ledger timestamp.
    pub fn get_deadlines(env: Env, vault_id: u64) -> Result<Deadlines, HeirVaultError> {
        vault::get_deadlines(&env, vault_id)
    }

    /// `true` when the vault's beneficiaries can claim right now.
    pub fn is_claimable(env: Env, vault_id: u64) -> Result<bool, HeirVaultError> {
        vault::is_claimable(&env, vault_id)
    }

    /// One page of an owner's vaults, newest ids in creation order.
    pub fn get_vaults_by_owner(
        env: Env,
        owner: Address,
        offset: u32,
        limit: u32,
    ) -> Result<VaultPage, HeirVaultError> {
        vault::get_by_owner(&env, owner, offset, limit)
    }

    /// Total number of vaults ever created.
    pub fn get_vault_count(env: Env) -> u64 {
        storage::vault_count(&env)
    }

    /// The schema version of the running contract code.
    ///
    /// Compare against a vault's own `schema_version` to detect a record written
    /// by an older layout.
    pub fn schema_version(_env: Env) -> u32 {
        SCHEMA_VERSION
    }
}
