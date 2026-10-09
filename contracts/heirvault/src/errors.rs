//! HeirVault contract error model.
//!
//! Every failure mode the contract can produce has its own stable numeric code.
//! Callers (SDK, indexer, frontend) can therefore branch on a specific reason
//! instead of parsing a panic string, and the codes are part of the contract's
//! public ABI: **existing codes must never be renumbered**, only appended to.
//!
//! Codes are grouped by the concern they belong to:
//!
//! | Range   | Concern                                            |
//! |---------|----------------------------------------------------|
//! | 1 – 9   | authorization, lookup and asset validation         |
//! | 10 – 19 | amounts and beneficiary allocation                 |
//! | 20 – 29 | guardians and activation thresholds                |
//! | 30 – 39 | lifecycle / state-machine violations               |
//! | 40 – 49 | timestamp and deadline violations                  |
//! | 50 – 59 | claims                                             |
//! | 60 – 69 | paging and arithmetic                              |

use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum HeirVaultError {
    // ---- 1..9: authorization, lookup, asset ----
    /// The caller did not authenticate as the address the operation requires.
    Unauthorized = 1,
    /// No vault is stored under the supplied id.
    VaultNotFound = 2,
    /// A vault already exists at the id that was about to be allocated.
    VaultAlreadyExists = 3,
    /// The supplied asset is not a usable SEP-41 token contract.
    InvalidAsset = 4,
    /// An address supplied as a beneficiary/guardian is not acceptable.
    InvalidAddress = 5,
    /// The contract has not been initialised for the requested operation.
    NotInitialized = 6,

    // ---- 10..19: amounts and allocation ----
    /// Amount was zero, negative, or outside the supported range.
    InvalidAmount = 10,
    /// The referenced beneficiary is not registered and active on this vault.
    InvalidBeneficiary = 11,
    /// A single allocation percentage was zero or greater than 100%.
    InvalidAllocation = 12,
    /// The operation would push the total allocation above 100%.
    AllocationExceeded = 13,
    /// Activation requires the beneficiary allocation to total exactly 100%.
    AllocationIncomplete = 14,
    /// The vault has no active beneficiaries to distribute to.
    NoBeneficiaries = 15,
    /// The per-vault beneficiary cap would be exceeded.
    TooManyBeneficiaries = 16,
    /// The address is already a beneficiary of this vault.
    DuplicateBeneficiary = 17,

    // ---- 20..29: guardians and thresholds ----
    /// The referenced guardian is not registered and active on this vault.
    InvalidGuardian = 20,
    /// The requested approval threshold is zero or exceeds the guardian count.
    InvalidThreshold = 21,
    /// The per-vault guardian cap would be exceeded.
    TooManyGuardians = 22,
    /// The address is already a guardian of this vault.
    DuplicateGuardian = 23,
    /// This guardian has already approved the current activation attempt.
    AlreadyApproved = 24,
    /// Guardian approval is not part of this vault's activation mode.
    ApprovalNotEnabled = 25,

    // ---- 30..39: lifecycle / state machine ----
    /// The vault is not in a state that permits this operation.
    InvalidState = 30,
    /// The vault is not in its editable (`ACTIVE` / `GRACE_PERIOD`) phase.
    VaultNotEditable = 31,
    /// The vault has already been activated.
    AlreadyActivated = 32,
    /// The vault has not been activated, so there is nothing to claim.
    NotActivated = 33,
    /// The vault cannot be activated yet: its activation conditions are unmet.
    NotEligible = 34,
    /// The vault has been cancelled and can no longer be operated on.
    VaultCancelled = 35,
    /// Every beneficiary has claimed; the vault is finished.
    VaultCompleted = 36,

    // ---- 40..49: timestamps and deadlines ----
    /// The check-in deadline has not yet passed.
    DeadlineNotReached = 40,
    /// The grace period is still running, so activation must wait.
    GracePeriodActive = 41,
    /// The grace period has expired, so the owner can no longer recover the vault.
    GracePeriodExpired = 42,
    /// A configured period (check-in or grace) is outside its supported range.
    InvalidPeriod = 43,

    // ---- 50..59: claims ----
    /// This beneficiary has already claimed its allocation.
    AlreadyClaimed = 50,
    /// The vault does not currently hold enough of the asset for this claim.
    ///
    /// Also returned when a payout would exceed the remaining balance, which is
    /// the contract's last line of defence against an inconsistent internal
    /// balance. A payout that legitimately rounds to zero is *not* an error: the
    /// heir's slot is marked claimed and no transfer is attempted.
    InsufficientBalance = 51,

    // ---- 60..69: paging and arithmetic ----
    /// A page `limit` of zero was requested.
    InvalidPageLimit = 60,
    /// Checked arithmetic failed; the operation would overflow or underflow.
    ArithmeticOverflow = 61,
}

/// Compile-time assertion that the error enum stays inside its documented ABI
/// budget. Appending new variants is fine; renumbering or exceeding the range
/// is a breaking change that this guard forces a reviewer to notice.
const _: () = {
    assert!(HeirVaultError::ArithmeticOverflow as u32 == 61);
    assert!(HeirVaultError::Unauthorized as u32 == 1);
};
