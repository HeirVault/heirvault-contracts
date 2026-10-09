//! Core value types shared by every HeirVault module.
//!
//! Everything in this module is a `#[contracttype]`, so it crosses the Soroban
//! host boundary as self-describing XDR rather than an opaque blob. Keeping the
//! stored layout strongly typed is what makes a future schema version
//! *detectable*: a migrator can deserialise an old `Vault`, inspect
//! [`Vault::schema_version`], and rewrite it, instead of guessing at a loosely
//! structured map of `Val`s.

use soroban_sdk::{contracttype, Address, Vec};

/// The allocation unit used across HeirVault. `10_000` basis points == 100%.
pub const BPS_DENOMINATOR: u32 = 10_000;

/// A vault must be allocated exactly to this total before it can be activated.
pub const FULL_ALLOCATION_BPS: u32 = BPS_DENOMINATOR;

/// Upper bound on beneficiaries per vault.
///
/// This is a safety property, not a product limitation. `activate_vault` and
/// `claim` both walk the beneficiary list, and an unbounded list would
/// eventually stop fitting inside a single transaction's CPU and ledger-entry
/// budget. At that point a vault could be activated but never fully claimed,
/// permanently stranding funds.
pub const MAX_BENEFICIARIES: u32 = 10;

/// Upper bound on guardians per vault. Same rationale as [`MAX_BENEFICIARIES`].
pub const MAX_GUARDIANS: u32 = 5;

/// Hard ceiling on the number of records any paginated view returns.
///
/// Callers may ask for less; asking for more is clamped rather than rejected so
/// that a naive client can never blow up its own simulation budget.
pub const MAX_PAGE_LIMIT: u32 = 25;

/// Shortest supported check-in period (1 minute).
///
/// The floor exists so a vault can never be created with a period of zero,
/// which would make every deadline already past at creation time.
pub const MIN_CHECK_IN_PERIOD: u64 = 60;

/// Longest supported check-in period (5 years).
pub const MAX_CHECK_IN_PERIOD: u64 = 5 * 365 * 24 * 60 * 60;

/// Shortest supported grace period (1 minute). Must be non-zero: a zero-length
/// grace period would remove the owner's only recovery window.
pub const MIN_GRACE_PERIOD: u64 = 60;

/// Longest supported grace period (1 year).
pub const MAX_GRACE_PERIOD: u64 = 365 * 24 * 60 * 60;

/// Schema version stamped into every vault at creation time.
///
/// Bump this whenever the stored [`Vault`] layout changes. Because the version
/// is stored *inside* each vault, a future migration entry point can tell which
/// vaults predate a layout change without any out-of-band bookkeeping.
pub const SCHEMA_VERSION: u32 = 1;

/// Lifecycle state of a vault.
///
/// The state machine is intentionally small and each state has an exact set of
/// legal operations (enforced in this crate, never in a frontend):
///
/// | State          | deposit | edit beneficiaries/guardians | check_in | cancel/withdraw | claim |
/// |----------------|---------|------------------------------|----------|-----------------|-------|
/// | `Active`       | yes     | yes                          | yes      | yes             | no    |
/// | `GracePeriod`  | yes     | yes                          | yes      | yes             | no    |
/// | `Activated`    | no      | no                           | no       | no              | yes   |
/// | `Cancelled`    | no      | no                           | no       | withdraw only   | no    |
/// | `Completed`    | no      | no                           | no       | no              | no    |
///
/// `Active` and `GracePeriod` are additionally time-derived: `GracePeriod`
/// begins as soon as `now >= deadline`, even if no transaction has been sent in
/// the meantime. Read entry points report that derived value, and the first
/// mutating entry point to observe it persists it.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VaultStatus {
    /// Owner is checking in on time; vault is fully editable.
    Active,
    /// The check-in deadline passed. The owner may still recover the vault by
    /// checking in, until the grace period expires.
    GracePeriod,
    /// Inheritance has been triggered. Configuration is frozen and
    /// beneficiaries may claim their allocations.
    Activated,
    /// The owner cancelled the vault. Remaining funds belong to the owner.
    Cancelled,
    /// Every beneficiary has claimed. The vault is finished and holds nothing.
    Completed,
}

impl VaultStatus {
    /// `true` once the vault can no longer change state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, VaultStatus::Cancelled | VaultStatus::Completed)
    }

    /// `true` while the vault can still be funded.
    pub fn accepts_deposits(&self) -> bool {
        matches!(self, VaultStatus::Active | VaultStatus::GracePeriod)
    }

    /// `true` while the owner may still change beneficiaries and guardians.
    pub fn is_editable(&self) -> bool {
        matches!(self, VaultStatus::Active | VaultStatus::GracePeriod)
    }

    /// `true` once beneficiaries are allowed to claim.
    pub fn is_claimable(&self) -> bool {
        matches!(self, VaultStatus::Activated)
    }
}

/// Which condition (or combination of conditions) releases a vault.
///
/// HeirVault deliberately exposes the mode as explicit data rather than
/// inferring behaviour from guardian configuration, so the activation rules a
/// vault was created under stay auditable for its whole life.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationMode {
    /// Release once `now >= deadline + grace_period`. Guardians are ignored.
    MissedCheckIn,
    /// Release as soon as the configured guardian threshold has approved,
    /// regardless of the check-in deadline. Useful when the owner is known to
    /// be incapacitated rather than merely inactive.
    GuardianApproval,
    /// Release only once *both* the grace period has expired *and* the guardian
    /// threshold has approved. The most conservative mode.
    MultiCondition,
}

impl ActivationMode {
    /// `true` if this mode requires the guardian threshold to be satisfied.
    pub fn requires_guardian_approval(&self) -> bool {
        matches!(
            self,
            ActivationMode::GuardianApproval | ActivationMode::MultiCondition
        )
    }

    /// `true` if this mode requires the check-in grace period to have expired.
    pub fn requires_missed_check_in(&self) -> bool {
        matches!(
            self,
            ActivationMode::MissedCheckIn | ActivationMode::MultiCondition
        )
    }
}

/// A single heir's slot inside a vault.
///
/// Slots are never deleted: `remove_beneficiary` flips [`Beneficiary::active`]
/// to `false` and zeroes the allocation. Keeping the slot is what makes claim
/// history auditable after a beneficiary is dropped, and it lets the same
/// address be re-added later without a duplicate slot.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Beneficiary {
    /// Heir's Stellar account or contract address.
    pub address: Address,
    /// Share of the distribution balance, in basis points.
    pub allocation_bps: u32,
    /// `false` once removed by the owner.
    pub active: bool,
    /// `true` once this heir has withdrawn its allocation.
    pub claimed: bool,
    /// Total amount this heir has actually received.
    pub claimed_amount: i128,
}

/// A guardian's slot inside a vault, plus its vote on the current activation
/// attempt.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Guardian {
    /// Guardian's Stellar account or contract address.
    pub address: Address,
    /// `false` once removed by the owner.
    pub active: bool,
    /// Vote on the *current* activation attempt. Reset to `false` by every
    /// owner check-in, so a stale approval can never be replayed.
    pub approved: bool,
}

/// The complete stored state of one inheritance vault.
///
/// Field ordering is part of the XDR layout; append new fields at the end and
/// bump [`SCHEMA_VERSION`] when the layout changes.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Vault {
    /// Monotonic identifier, unique for the lifetime of the contract.
    pub id: u64,
    /// The only address allowed to configure, check in, cancel and withdraw.
    pub owner: Address,
    /// SEP-41 token contract this vault accepts and distributes.
    pub asset: Address,
    /// Internally tracked amount of `asset` held on this vault's behalf.
    ///
    /// Always equals `distribution_balance - total_claimed` once activated.
    pub balance: i128,
    /// Persisted lifecycle state. See [`VaultStatus`] for the time-derived part.
    pub status: VaultStatus,
    /// Which rules release this vault.
    pub activation_mode: ActivationMode,

    /// Maximum time the owner may go without checking in, in seconds.
    pub check_in_period: u64,
    /// Recovery window after the check-in deadline, in seconds.
    pub grace_period: u64,
    /// Ledger timestamp of the most recent successful check-in.
    pub last_check_in: u64,
    /// Ledger timestamp of the most recent status change, for off-chain auditing.
    pub last_status_change: u64,

    /// Ledger timestamp at which the vault was activated (`0` while unactivated).
    pub activated_at: u64,
    /// Balance snapshot taken at activation. Claim amounts are calculated
    /// against this value and never against the live balance, so a later
    /// transfer can never change what an heir is owed.
    pub distribution_balance: i128,
    /// Sum of every successful claim so far.
    pub total_claimed: i128,
    /// Number of heirs that have completed their claim.
    pub claimed_count: u32,

    /// Ordered heir slots (inactive slots retained for history).
    pub beneficiaries: Vec<Beneficiary>,
    /// Ordered guardian slots.
    pub guardians: Vec<Guardian>,
    /// Approvals required to release a guardian-gated vault.
    pub guardian_threshold: u32,
    /// Approvals recorded for the *current* activation attempt.
    pub guardian_approvals: u32,

    /// Ledger timestamp at creation.
    pub created_at: u64,
    /// Layout version of this stored record. See [`SCHEMA_VERSION`].
    pub schema_version: u32,
}

impl Vault {
    /// Ledger timestamp after which the owner is overdue.
    ///
    /// Uses saturating arithmetic: the configured periods are range-checked at
    /// creation, so this can only saturate if a vault were somehow written with
    /// values outside those ranges, in which case "never overdue" is the safe
    /// interpretation.
    pub fn deadline(&self) -> u64 {
        self.last_check_in.saturating_add(self.check_in_period)
    }

    /// Ledger timestamp after which the grace period is over.
    ///
    /// The grace window is the half-open interval `[deadline, grace_end)`:
    /// `deadline` itself is already overdue, and `grace_end` is already too
    /// late for a check-in or a cancellation.
    pub fn grace_end(&self) -> u64 {
        self.deadline().saturating_add(self.grace_period)
    }

    /// `true` when the owner's recovery window has closed.
    pub fn grace_period_expired(&self, now: u64) -> bool {
        now >= self.grace_end()
    }

    /// The state implied by the clock, which may be one step ahead of
    /// [`Vault::status`] because Soroban state only changes when a transaction
    /// writes to it.
    pub fn effective_status(&self, now: u64) -> VaultStatus {
        match self.status {
            VaultStatus::Active if now >= self.deadline() => VaultStatus::GracePeriod,
            other => other,
        }
    }

    /// Number of beneficiary slots that are still active.
    pub fn active_beneficiaries(&self) -> u32 {
        let mut count = 0;
        for slot in self.beneficiaries.iter() {
            if slot.active {
                count += 1;
            }
        }
        count
    }

    /// Sum of every active beneficiary's allocation, in basis points.
    pub fn total_allocation_bps(&self) -> u32 {
        let mut total = 0;
        for slot in self.beneficiaries.iter() {
            if slot.active {
                total += slot.allocation_bps;
            }
        }
        total
    }

    /// Number of guardian slots that are still active.
    pub fn active_guardians(&self) -> u32 {
        let mut count = 0;
        for slot in self.guardians.iter() {
            if slot.active {
                count += 1;
            }
        }
        count
    }

    /// Index of the active beneficiary slot for `address`, if any.
    ///
    /// A linear scan is safe here and everywhere else in this crate: the list
    /// is capped by [`MAX_BENEFICIARIES`] / [`MAX_GUARDIANS`], so a lookup is at
    /// most ten slot comparisons and has no unbounded-loop risk.
    pub fn beneficiary_index(&self, address: &Address) -> Option<u32> {
        for (index, slot) in self.beneficiaries.iter().enumerate() {
            if slot.active && &slot.address == address {
                return Some(index as u32);
            }
        }
        None
    }

    /// Index of the guardian slot for `address`, if any (active or not).
    pub fn guardian_index(&self, address: &Address) -> Option<u32> {
        for (index, slot) in self.guardians.iter().enumerate() {
            if &slot.address == address {
                return Some(index as u32);
            }
        }
        None
    }

    /// Compact projection of this vault for list endpoints.
    pub fn to_summary(&self, now: u64) -> VaultSummary {
        VaultSummary {
            id: self.id,
            owner: self.owner.clone(),
            asset: self.asset.clone(),
            balance: self.balance,
            status: self.effective_status(now),
            activation_mode: self.activation_mode,
            check_in_period: self.check_in_period,
            grace_period: self.grace_period,
            last_check_in: self.last_check_in,
            deadline: self.deadline(),
            grace_end: self.grace_end(),
            beneficiary_count: self.active_beneficiaries(),
            guardian_count: self.active_guardians(),
            total_allocation_bps: self.total_allocation_bps(),
            schema_version: self.schema_version,
        }
    }
}

/// Compact projection of [`Vault`] used by list endpoints.
///
/// Returning this instead of the full vault keeps paged reads cheap: a page of
/// twenty vaults never has to serialise twenty beneficiary vectors.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultSummary {
    pub id: u64,
    pub owner: Address,
    pub asset: Address,
    pub balance: i128,
    /// Clock-adjusted status, so clients never have to re-derive deadlines.
    pub status: VaultStatus,
    pub activation_mode: ActivationMode,
    pub check_in_period: u64,
    pub grace_period: u64,
    pub last_check_in: u64,
    /// `last_check_in + check_in_period`.
    pub deadline: u64,
    /// `deadline + grace_period`.
    pub grace_end: u64,
    pub beneficiary_count: u32,
    pub guardian_count: u32,
    pub total_allocation_bps: u32,
    pub schema_version: u32,
}

/// Full detail for a single vault, including the values a client would
/// otherwise have to recompute.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultInfo {
    /// The raw stored record.
    pub vault: Vault,
    /// `last_check_in + check_in_period`.
    pub deadline: u64,
    /// `deadline + grace_period`.
    pub grace_end: u64,
    /// Status adjusted for the current ledger timestamp.
    pub effective_status: VaultStatus,
    /// Sum of active allocations, in basis points.
    pub total_allocation_bps: u32,
    /// Approvals needed for a guardian-gated release.
    pub guardian_threshold: u32,
    /// Approvals recorded so far for the current attempt.
    pub guardian_approvals: u32,
    /// `true` when this vault can be activated at the current ledger time,
    /// assuming the guardian condition (if any) is already satisfied.
    pub activation_ready: bool,
}

/// One heir's claim position, as reported by [`crate::claims`].
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimRecord {
    pub beneficiary: Address,
    pub allocation_bps: u32,
    /// Share of the distribution balance before rounding.
    ///
    /// Rounding dust (the difference between the sum of floored shares and the
    /// distribution balance) is paid to the last heir to claim, so the sum of
    /// every heir's actual payout is exactly `distribution_balance`.
    pub entitlement: i128,
    /// Amount actually transferred to this heir so far.
    pub claimed_amount: i128,
    pub claimed: bool,
}

/// Paging metadata shared by every list endpoint.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageMeta {
    /// Total number of records in the collection, ignoring the requested page.
    pub total: u32,
    /// Offset this page started at.
    pub offset: u32,
    /// Maximum number of records this page could contain (after clamping).
    pub limit: u32,
    /// `true` when at least one record exists beyond this page.
    pub has_more: bool,
    /// Offset to pass to fetch the next page, or `None` on the last page.
    pub next_offset: Option<u32>,
}

/// A page of [`VaultSummary`] records.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultPage {
    pub items: Vec<VaultSummary>,
    pub meta: PageMeta,
}

/// A page of beneficiary slots.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BeneficiaryPage {
    pub items: Vec<Beneficiary>,
    pub meta: PageMeta,
}

/// A page of guardian slots.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardianPage {
    pub items: Vec<Guardian>,
    pub meta: PageMeta,
}

/// A page of claim positions.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimPage {
    pub items: Vec<ClaimRecord>,
    pub meta: PageMeta,
}

/// Live guardian-approval position for one vault.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardianStatus {
    /// Approvals recorded for the current activation attempt.
    pub approvals: u32,
    /// Approvals required to release a guardian-gated vault.
    pub threshold: u32,
    /// Number of active guardians.
    pub guardian_count: u32,
    /// `true` once `approvals >= threshold`, with a non-zero threshold and at
    /// least one guardian. This alone does not mean the vault can be activated:
    /// [`crate::types::ActivationMode::MultiCondition`] also needs the grace
    /// period to have expired.
    pub threshold_met: bool,
}

/// The two timestamps that drive the check-in and grace machinery.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Deadlines {
    /// `last_check_in + check_in_period`.
    pub deadline: u64,
    /// `deadline + grace_period`.
    pub grace_end: u64,
    /// Current ledger timestamp at the moment of the read.
    pub now: u64,
}
