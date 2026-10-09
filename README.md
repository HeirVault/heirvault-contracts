# HeirVault

**A programmable digital inheritance vault for Stellar / Soroban.**

HeirVault lets anyone lock supported SEP-41 assets (USDC, or any other SEP-41
token) into a smart contract, name beneficiaries with percentage shares, and set
a check-in schedule. As long as the owner keeps checking in, the vault is theirs
and only theirs. If they stop — because they are travelling, incapacitated, or
gone — the vault opens to their heirs, who claim their shares directly from the
contract. No lawyer, no court, no custodian.

> **Status: audited-by-nobody, deployed to Testnet.** Everything marked
> **Implemented** below is exercised by the automated test suite *and* by real
> Testnet transactions (see [Testnet deployment](#testnet-deployment)). Anything
> not implemented is listed under [Planned](#planned-not-implemented) and is
> deliberately absent from the contract rather than stubbed out.

---

## Table of contents

- [Why it exists](#why-it-exists)
- [What HeirVault does differently](#what-heirvault-does-differently)
- [Architecture](#architecture)
- [Vault lifecycle](#vault-lifecycle)
- [State machine](#state-machine)
- [Beneficiary model](#beneficiary-model)
- [Guardian model](#guardian-model)
- [Check-in system](#check-in-system)
- [Activation mechanism](#activation-mechanism)
- [Claim mechanism](#claim-mechanism)
- [Cancellation and withdrawal](#cancellation-and-withdrawal)
- [Storage architecture](#storage-architecture)
- [Events](#events)
- [Error codes](#error-codes)
- [Security considerations](#security-considerations)
- [Resource usage](#resource-usage)
- [Local development](#local-development)
- [Testing](#testing)
- [Building](#building)
- [Deployment](#deployment)
- [Testnet deployment](#testnet-deployment)
- [Contract invocation examples](#contract-invocation-examples)
- [Planned (not implemented)](#planned-not-implemented)
- [Remaining work](#remaining-work)

---

## Why it exists

Inheritance is a solvency problem disguised as an estate-planning problem. Funds
held at an address are only spendable by a private key, and when the keyholder
dies the funds do not become "theirs" — they become *nobody's*, permanently. A
self-custodied portfolio is destroyed by a single event that no counterparty can
observe on-chain.

HeirVault makes "the owner is gone" into a checkable on-chain fact. The owner
proves liveness by transacting on a schedule; the contract converts a lapse in
that schedule into a transfer of entitlement. The result is an instrument that is
unilaterally cancellable while the owner lives, and unstoppable once they are
gone.

---

## What HeirVault does differently

HeirVault was designed by studying the public `SoroWill/sorowill-contracts`
project as an architectural reference for what a working Soroban inheritance
protocol looks like, then built independently. The design differs in several
deliberate ways:

| Concern | SoroWill-style approach | HeirVault |
|---|---|---|
| **Funding** | Funds are locked as part of creating the will | Creation and funding are **separate**: `create_vault` then `deposit`, repeatable |
| **Release** | One transaction pays every beneficiary | **Pulled claims**: each heir claims independently, so one blocked heir cannot strand the others |
| **Guardian gate** | Guardians force an early release | The gate is an explicit, enforced `ActivationMode`, and a vault whose mode does not consult guardians *rejects* guardian votes instead of recording meaningless ones |
| **State model** | `Active → Triggered → Released` | Five explicit states with a documented legal-operation table, plus time-derived transitions |
| **Storage** | Single will record | Versioned vault record + a point-read owner index for paging |

The most important difference is the release mechanism. A push-based release
must fit *every* heir's transfer into one transaction's CPU and ledger budget.
That imposes a hard ceiling, and — worse — a single un-transferable heir (frozen
account, rejecting contract) blocks everyone else's payout. HeirVault's pulled
claims make each heir's payout an independent transaction.

---

## Architecture

```
heirvault-contracts/
├── contracts/heirvault/
│   ├── src/
│   │   ├── lib.rs             # #[contractimpl]: the public ABI, thin delegation
│   │   ├── types.rs           # Vault, Beneficiary, Guardian, enums, page types
│   │   ├── errors.rs          # Stable numeric error ABI
│   │   ├── storage.rs         # DataKey layout, TTL, versioning, owner index
│   │   ├── events.rs          # One typed helper per event
│   │   ├── vault.rs           # create, deposit, check_in, activate, cancel, withdraw
│   │   ├── beneficiaries.rs   # add / remove / update + allocation invariant
│   │   ├── guardians.rs       # M-of-N approval + anti-lockout rules
│   │   ├── claims.rs          # claim, rounding, double-claim prevention
│   │   └── test.rs            # 123 in-crate tests
│   └── Cargo.toml
├── tests/                     # 9 external integration tests (public ABI only)
│   ├── Cargo.toml
│   ├── src/lib.rs
│   └── integration/end_to_end.rs
├── docs/
│   ├── STORAGE.md
│   ├── SECURITY.md
│   ├── RESOURCE_COSTS.md
│   └── TESTNET.md
├── Cargo.toml                 # workspace
├── README.md
└── .gitignore
```

`lib.rs` contains no business logic. It declares the ABI and delegates to the
modules, where the logic can be unit tested without crossing the contract
boundary. Each module owns one concern and documents the invariants it enforces.

---

## Vault lifecycle

```
                       owner checks in (before deadline)
                    ┌──────────────────────────────────────┐
                    ▼                                      │
             ┌────────────┐   now >= deadline    ┌────────────────┐
   create ──▶│   ACTIVE   │─────────────────────▶│  GRACE_PERIOD  │
             └────────────┘                      └────────────────┘
                    │                               │        │
       owner cancel │                    owner checks │        │ now >= grace_end
                    │                    in (recovers)│        │ + guardian condition
                    │                               │        │
                    │                               └────────┼──────┐
                    │                                        │      │
                    │                          owner cancel? │      ▼
                    │                                        │  ┌────────────┐
                    │                                        │  │ ACTIVATED  │
                    ▼                                        ▼  └────────────┘
             ┌────────────┐                            ┌────────────┐   │ all heirs
             │ CANCELLED  │                            │ CANCELLED  │   │ claim
             └────────────┘                            └────────────┘   ▼
                  │                                                 ┌────────────┐
                  │ withdraw (partial or full)                      │ COMPLETED  │
                  ▼                                                 └────────────┘
              funds → owner
```

---

## State machine

Five states, each with an exact set of legal operations. **Every rule in this
table is enforced inside the contract**; a frontend that disagrees will simply
get a reverted transaction.

| State | deposit | edit beneficiaries / guardians | check_in | cancel | withdraw | claim | activate |
|---|---|---|---|---|---|---|---|
| `ACTIVE` | ✅ | ✅ | ✅ | ✅ | ❌ | ❌ | ❌ *(deadline not reached)* |
| `GRACE_PERIOD` | ✅ | ✅ | ✅ *(until `grace_end`)* | ✅ | ❌ | ❌ | ✅ *(once `grace_end` passes)* |
| `ACTIVATED` | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ | ❌ |
| `CANCELLED` | ❌ | ❌ | ❌ | ❌ | ✅ | ❌ | ❌ |
| `COMPLETED` | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |

Worked examples of the rules this enforces:

- A cancelled vault cannot receive deposits → `VaultCancelled`.
- A beneficiary cannot claim twice → `AlreadyClaimed`, or `VaultCompleted` once
  the vault is settled.
- An activated vault cannot behave like an active vault: no deposits, no
  beneficiary edits, no check-in, no cancellation → `AlreadyActivated`.
- An activated vault cannot be modified by its owner → all configuration
  entry points return `AlreadyActivated`, not a silent no-op.
- A beneficiary cannot claim before the vault is activated → `NotActivated`.

`ACTIVE → GRACE_PERIOD` is **time-derived**: it becomes true the moment the
ledger timestamp reaches the deadline, whether or not a transaction has been
sent. Soroban state only changes when something writes to it, so read entry
points report the derived value (`get_vault`, `get_vault_status`,
`get_deadlines`) and the first mutating call to observe the transition persists
it. Either way, no operation can succeed under the wrong rule.

---

## Beneficiary model

Each beneficiary is a slot:

| Field | Meaning |
|---|---|
| `address` | Heir's account or contract address |
| `allocation_bps` | Share in basis points; `10_000` = 100% |
| `active` | `false` once removed |
| `claimed` | `true` once paid |
| `claimed_amount` | Total actually received |

**Allocation is stored in basis points, never floats.** The invariant enforced by
every mutator is:

```text
sum(active allocations) <= 10_000
```

The strict form `== 10_000` is enforced exactly once, at activation. That split
is deliberate: an owner must be able to build a list up one entry at a time and
must be able to remove someone without being forced to immediately rebalance,
but **every activated vault is exactly 100% allocated**.

Slots are **retained, not deleted**, when a beneficiary is removed. The
consequences, all intentional:

1. The on-chain record of who was ever entitled survives removal — useful when a
   dispute years later turns on the configuration at a point in time.
2. Re-adding the same address reactivates its existing slot, so an address can
   never appear twice in a vault.
3. A vault is capped at `MAX_BENEFICIARIES` (10) **distinct addresses over its
   lifetime**. Owners who need more history create another vault.

`address` may not equal the owner or the contract itself (`InvalidAddress`).

---

## Guardian model

Guardians are an *additional* release path for the case where the owner is
incapacitated rather than merely inactive. They never replace the check-in
mechanism; the vault's `ActivationMode` decides whether they are consulted:

| `ActivationMode` | Requires missed check-in | Requires guardian threshold |
|---|---|---|
| `MissedCheckIn` | ✅ | ❌ |
| `GuardianApproval` | ❌ | ✅ |
| `MultiCondition` | ✅ | ✅ |

Rules, all enforced:

- Only the owner may add, remove, or set the threshold (`Unauthorized`).
- Duplicates are rejected (`DuplicateGuardian`); slots are retained like
  beneficiary slots.
- The first guardian added sets the threshold to `1`; the owner raises it
  explicitly.
- `1 <= threshold <= active guardians` (`InvalidThreshold`).
- An approval is a flag on the guardian's **own** slot, never a blindly
  incremented counter. `guardian_approvals` is always recomputed from the slots,
  so it is a count of *distinct* guardians by construction.
- A guardian may approve **once per activation attempt** (`AlreadyApproved`).
- **Every owner check-in clears every approval flag.** A vote cast before the
  owner proved they were alive can never be replayed against a later attempt.
- A vault whose mode does not consult guardians *rejects* approvals with
  `ApprovalNotEnabled` rather than recording votes that mean nothing.
- `guardian_approve` is not enough to release: a separate permissionless
  `activate_vault` re-derives every condition. Keeping the two apart means each
  path has exactly one place to audit.

### Anti-lockout invariant

Removing a guardian, and lowering the threshold, are both refused when they would
leave a guardian-gated vault unable to *ever* reach its threshold — no guardians
left, or a zero threshold. Without that check an owner could strand a vault's
funds where no beneficiary could ever claim them, which is precisely the failure
an inheritance contract exists to prevent.

---

## Check-in system

```text
deadline  = last_check_in + check_in_period
grace_end = deadline      + grace_period
```

Boundary behaviour, defined once and enforced everywhere. The intervals are
**half-open**:

| `now` | Effective state | `check_in` | `cancel` | `activate_vault` |
|---|---|---|---|---|
| `now < deadline` | `ACTIVE` | ✅ | ✅ | `DeadlineNotReached` |
| `now == deadline` | `GRACE_PERIOD` | ✅ | ✅ | `GracePeriodActive` |
| `deadline < now < grace_end` | `GRACE_PERIOD` | ✅ | ✅ | `GracePeriodActive` |
| `now == grace_end` | `GRACE_PERIOD` (expired) | `GracePeriodExpired` | ✅ | ✅ |
| `now > grace_end` | `GRACE_PERIOD` (expired) | `GracePeriodExpired` | ✅ | ✅ |

Two consequences worth stating explicitly:

- **A check-in at exactly `deadline` does not save the vault from
  `GRACE_PERIOD`.** The deadline is the first overdue instant, not the last valid
  one. This removes the ambiguity of "is it inclusive?" from every comparison.
- **Activation is the one comparison that is inclusive at `grace_end`.** There is
  therefore no timestamp at which a vault is neither recoverable nor activatable.

All comparisons use `env.ledger().timestamp()`, never a caller-supplied value, so
a client cannot choose which moment it is evaluated at.

Only the owner can `check_in` — that is the entire point. Nobody else may reset
the timer and indefinitely postpone an inheritance. A successful check-in:

1. verifies the caller is the owner,
2. verifies the vault is in `ACTIVE` or `GRACE_PERIOD`,
3. verifies the grace period has **not** expired,
4. sets `last_check_in = now`, restarts the deadline,
5. returns the vault to `ACTIVE`,
6. clears every guardian approval,
7. emits `check_in_completed`.

Periods are range-checked at creation: `60s <= check_in_period <= 5 years` and
`60s <= grace_period <= 1 year`. The floors exist so a vault can never be created
with a zero period, which would make every deadline already past at creation.
Deployments intended for humans should use much larger values.

---

## Activation mechanism

`activate_vault(vault_id)` is **permissionless**. Once the conditions hold,
anyone may push the vault forward. Requiring the owner to call it would be
self-defeating (the premise is that the owner is gone), and requiring a specific
keeper would introduce a liveness dependency. The permission is safe because the
function re-derives every condition from on-chain state and the ledger clock, and
because it **cannot pay anyone** — it only flips the state that makes claims
possible.

All conditions, enforced:

1. the vault is not already `ACTIVATED`, `CANCELLED` or `COMPLETED`;
2. the mode's clock condition is satisfied (`MissedCheckIn` / `MultiCondition`);
3. the mode's guardian condition is satisfied (`GuardianApproval` /
   `MultiCondition`);
4. there is at least one active beneficiary (`NoBeneficiaries`);
5. active allocations total exactly 100% (`AllocationIncomplete`);
6. there is a non-zero balance to distribute (`InsufficientBalance`).

On success the balance is snapshotted into `distribution_balance` and the status
becomes `ACTIVATED`. Claims are computed against that snapshot, so what each heir
is owed is fixed at the moment of activation and cannot be shifted by later
transfers or claim timing. Configuration is frozen from this point on.

`get_vault` returns an `activation_ready` boolean derived from the same shared
helper the contract itself uses, so what a client is shown can never disagree
with what the contract will accept.

---

## Claim mechanism

```text
entitlement(heir) = floor(distribution_balance * allocation_bps / 10_000)
```

Integer division loses a remainder — "dust" that would otherwise be locked
forever. HeirVault gives it to **the last heir to claim**, who receives the
vault's remaining balance instead of its floored share. That buys a strong,
testable property:

> `sum(all payouts) == distribution_balance`, and the vault's balance reaches
> exactly zero when the last heir claims.

Verified on Testnet: a `1_000_000_001` deposit split 3333 / 3333 / 3334 bps paid
`333_300_000` / `333_300_000` / `333_400_001` — exactly the deposited amount, with
the contract left holding `0`.

Validation on every claim:

| Check | Error |
|---|---|
| Vault is `ACTIVATED` | `NotActivated` / `VaultCompleted` / `VaultCancelled` |
| Caller is an active beneficiary | `InvalidBeneficiary` |
| Caller has not already claimed | `AlreadyClaimed` |
| Payout does not exceed the remaining balance | `InsufficientBalance` |

**Double-claim safety.** The `claimed` flag is set *before* the transfer, and
there is no path that pays an heir twice. Because a reverted transfer reverts the
flag with it, a failed claim leaves the heir able to try again but never able to
be paid twice. This was observed on Testnet: two claims reverted at the token
layer (the heirs had no trustline for the issued asset) and the subsequent
retries succeeded with correct accounting — the revert did not corrupt claim
state.

When the last heir claims, the vault moves to `COMPLETED` and loses its balance.

---

## Cancellation and withdrawal

```text
cancel_vault(vault_id)      # owner only; ACTIVE or GRACE_PERIOD
withdraw(vault_id, amount)  # owner only; CANCELLED only
```

- Cancellation moves **no funds**. It flips the state that makes `withdraw`
  legal, so a cancelled vault's balance stays visible and only shrinks through
  explicit, individually logged withdrawals. Partial withdrawals are supported.
- `withdraw` cannot exceed the balance (`InsufficientBalance`) and rejects
  non-positive amounts (`InvalidAmount`).
- **A cancellation can never interrupt an active claim process:** once a vault is
  `ACTIVATED`, `cancel_vault` returns `AlreadyActivated` and `withdraw` returns
  `AlreadyActivated` too.
- Cancellation is allowed in `GRACE_PERIOD` even after `grace_end`, as long as
  nobody has activated the vault. The reasoning: cancellation requires the
  owner's signature, so an owner who cancels is by definition alive, and
  releasing a living owner's funds to their heirs would be wrong. Entitlement
  only ever transfers on an explicit activation. The alternative would be to lock
  a living owner out of their own funds, which is the worse failure.

---

## Storage architecture

Full detail, including the migration procedure, is in **[docs/STORAGE.md](docs/STORAGE.md)**.

Four ledger entries, all keyed by a single `DataKey` enum:

| Key | Durability | Written by | Purpose |
|---|---|---|---|
| `NextVaultId` | instance | `create_vault` | monotonic vault id allocator |
| `Vault(u64)` | persistent | most mutators | the vault record |
| `OwnerVaultCount(Address)` | persistent | `create_vault` | size of an owner's vault list |
| `OwnerVault(Address, u32)` | persistent | `create_vault` | one slot of an owner's vault list |

- **No temporary storage.** Every value a vault depends on must outlive a single
  transaction, otherwise a vault could become permanently unreadable.
- **Instance vs persistent:** the id counter is one small hot value, so it lives
  in the single instance entry. Vaults and the owner index are persistent so each
  carries its own rent and TTL — one owner's idle vault cannot expire another
  owner's active one.
- **TTL** is refreshed on every read and write of a vault entry (~30 day
  threshold, extended to ~90 days), so an actively used vault never expires.
- **Pagination without scans.** `get_vaults_by_owner` reads `limit` slots through
  the owner index; its cost is independent of how many vaults exist globally. The
  index is append-only, so offsets are stable across calls.
- **Schema versioning.** `Vault::schema_version` is stamped into every record at
  creation and `schema_version()` reports what the running code expects. A future
  layout change appends fields, bumps `SCHEMA_VERSION`, and adds a migration entry
  point that detects and rewrites old records. Nothing has to be migrated eagerly.

---

## Events

Every state-mutating entry point publishes **exactly one** event. Topic `[0]` is
a stable name, topic `[1]` is the vault id — so a subscriber can filter one
vault's whole history from a single ledger range — and the payload carries
everything else.

| Event | Emitted by | Payload |
|---|---|---|
| `vault_created` | `create_vault` | owner, asset, activation_mode, check_in_period, grace_period, created_at |
| `deposit_made` | `deposit` | depositor, amount, new balance |
| `beneficiary_added` | `add_beneficiary` | beneficiary, allocation_bps, new total bps |
| `beneficiary_removed` | `remove_beneficiary` | beneficiary, new total bps |
| `beneficiary_updated` | `update_beneficiary` | beneficiary, old bps, new bps, new total bps |
| `guardian_added` | `add_guardian` | guardian, guardian count, threshold |
| `guardian_removed` | `remove_guardian` | guardian, guardian count, threshold |
| `guardian_threshold_updated` | `set_guardian_threshold` | threshold, guardian count |
| `guardian_approved` | `guardian_approve` | guardian, approvals, threshold |
| `check_in_completed` | `check_in` | owner, last_check_in, next deadline |
| `vault_activated` | `activate_vault` | activation_mode, distribution_balance, beneficiary count, timestamp |
| `inheritance_claimed` | `claim` | beneficiary, amount, total claimed, remaining, timestamp |
| `vault_cancelled` | `cancel_vault` | owner, refundable balance |
| `withdrawal` | `withdraw` | owner, amount, remaining balance |
| `vault_completed` | `claim` (last heir) | total distributed, beneficiary count, timestamp |

`claim` emits two events on its final call (`inheritance_claimed` and
`vault_completed`) because the vault genuinely changes state twice; that is the
one documented exception to one-event-per-entry-point.

Payloads carry **identifiers and deltas**, not reconstructed state. A client that
needs the full vault reads it back with `get_vault`. This keeps event bytes —
which are charged as ledger writes — small and bounded regardless of how many
beneficiaries a vault has.

---

## Error codes

Every failure mode has a stable numeric code that is part of the public ABI.
Codes are grouped and **never renumbered**, only appended to.

| Range | Concern |
|---|---|
| 1–9 | authorization, lookup, asset |
| 10–19 | amounts and allocation |
| 20–29 | guardians and thresholds |
| 30–39 | lifecycle / state machine |
| 40–49 | timestamps and deadlines |
| 50–59 | claims |
| 60–69 | paging and arithmetic |

<details>
<summary>Full list</summary>

| Code | Error | Meaning |
|---|---|---|
| 1 | `Unauthorized` | caller did not authenticate as the required address |
| 2 | `VaultNotFound` | no vault at this id (or it is unreadable) |
| 3 | `VaultAlreadyExists` | id collision (defensive; the allocator is monotonic) |
| 4 | `InvalidAsset` | asset is not a usable SEP-41 token, or is the wrong one |
| 5 | `InvalidAddress` | beneficiary/guardian equals the owner or the contract |
| 6 | `NotInitialized` | reserved |
| 10 | `InvalidAmount` | zero, negative, or out-of-range amount |
| 11 | `InvalidBeneficiary` | not a registered active beneficiary |
| 12 | `InvalidAllocation` | zero or >100% single allocation |
| 13 | `AllocationExceeded` | would push the total above 100% |
| 14 | `AllocationIncomplete` | activation needs exactly 100% |
| 15 | `NoBeneficiaries` | nothing to distribute to |
| 16 | `TooManyBeneficiaries` | per-vault slot cap reached |
| 17 | `DuplicateBeneficiary` | address already an active beneficiary |
| 20 | `InvalidGuardian` | not a registered active guardian |
| 21 | `InvalidThreshold` | zero, above the guardian count, or would strand the vault |
| 22 | `TooManyGuardians` | per-vault guardian cap reached |
| 23 | `DuplicateGuardian` | address already a guardian |
| 24 | `AlreadyApproved` | this guardian already voted on the attempt |
| 25 | `ApprovalNotEnabled` | the vault's activation mode ignores guardians |
| 30 | `InvalidState` | operation not legal in the current state |
| 31 | `VaultNotEditable` | vault has left its editable phase |
| 32 | `AlreadyActivated` | vault is already `ACTIVATED` |
| 33 | `NotActivated` | nothing to claim yet |
| 34 | `NotEligible` | activation conditions unmet |
| 35 | `VaultCancelled` | vault was cancelled |
| 36 | `VaultCompleted` | vault is fully settled |
| 40 | `DeadlineNotReached` | check-in deadline is in the future |
| 41 | `GracePeriodActive` | grace period still running |
| 42 | `GracePeriodExpired` | the recovery window has closed |
| 43 | `InvalidPeriod` | configured period outside its supported range |
| 50 | `AlreadyClaimed` | this beneficiary already claimed |
| 51 | `InsufficientBalance` | payout exceeds the remaining balance |
| 60 | `InvalidPageLimit` | a page limit of zero |
| 61 | `ArithmeticOverflow` | checked arithmetic failed |

</details>

---

## Security considerations

A full threat-model write-up is in **[docs/SECURITY.md](docs/SECURITY.md)**. Summary
of the controls implemented:

**Authorization**
- Every privileged entry point calls `Address::require_auth()` on the address it
  *reads from state* — never on a caller-supplied parameter. The owner of a vault
  is the only address that can configure, check in, cancel or withdraw.
- `claim` requires the beneficiary's own signature; `guardian_approve` requires
  the guardian's.
- `activate_vault` is intentionally permissionless and therefore takes no
  authorization at all; it is safe because it only re-derives conditions from
  state and cannot move funds.
- 12 privileged entry points are tested against (a) a caller holding a *valid*
  authorization for a *different* address, and (b) no authorization at all.

**State and value**
- Explicit state machine; every illegal transition returns a typed error rather
  than being silently ignored.
- Allocation invariant `sum <= 10_000` on every mutator, `== 10_000` at
  activation.
- Guardian approval counted from per-guardian flags, never incremented blindly;
  votes reset on every owner check-in.
- Double-claim prevented by a flag set before the transfer, inside the same
  atomic transaction.
- Checks → effects → interactions everywhere: state is written *before* any
  token transfer, so a re-entrant or hostile token observes post-transfer
  accounting, and a failed transfer reverts the accounting with it. The internal
  balance can therefore never drift from the tokens actually held.
- Rounding dust is paid out rather than trapped, so a vault always reaches
  balance `0` and `COMPLETED`.
- `balance` is decremented with checked arithmetic and a payout is refused if it
  would exceed the remaining balance — the last line of defence against
  inconsistent accounting.

**Input validation**
- Assets are probe-checked with a real `decimals()` cross-contract call at
  creation, and `deposit` takes the asset explicitly so a wrong token is rejected
  *before* any transfer. (The probe is best-effort by nature; the non-negotiable
  check is that `deposit` performs a real SEP-41 transfer and reverts if it
  fails.)
- Amounts must be positive; periods range-checked; percentages validated in both
  directions; duplicate beneficiaries and guardians rejected; addresses equal to
  the owner or the contract rejected.
- Guardian configurations that would make a vault permanently unreleasable are
  rejected (anti-lockout).

**Resources**
- No unbounded loops or unbounded storage reads. Beneficiary and guardian lists
  are capped by constants, so every linear scan is bounded by 10 and 5. List
  endpoints are paginated with a hard ceiling of `MAX_PAGE_LIMIT` (25) records
  per call, clamped rather than rejected so a naive client cannot blow up its own
  simulation budget.
- Vault records carry an explicit TTL that every access refreshes, so a funded
  vault cannot be archived while it matters.

### Known limitations (documented, not hidden)

- **Timestamp trust.** Deadlines use `env.ledger().timestamp()`, which is the
  validator-set consensus time. On Stellar this is close to wall-clock and not
  attacker-controlled at the contract level, but a Soroban contract cannot
  enforce "wall clock" beyond that. Values far in the past or future would shift
  deadlines; the network's consensus rules are the mitigation.
- **No beneficiary consent.** Anyone can be named an heir; there is no
  accept/decline step. An heir cannot be forced to accept, but they also cannot
  block a distribution.
- **An owner can always pre-empt activation.** An owner who is alive, holds the
  key, and transacts before anyone calls `activate_vault` can cancel and recover
  the vault. This is a deliberate choice (see [Cancellation](#cancellation-and-withdrawal))
  and it means heirs never have a *guaranteed* claim while the owner can still
  sign. It favours the living over the anticipated dead.
- **Single-asset vaults.** Each vault holds exactly one SEP-41 asset.
- **No upgrade path in v1.** The contract is immutable once deployed. Adding one
  requires a deliberate design decision about who controls it, so it is left out
  rather than added speculatively. Schema versioning exists so a *future*
  migration entry point can be added without breaking existing records.
- **`VaultNotFound` conflates cases.** Soroban's persistent-storage API does not
  expose *why* a key is absent, so the error cannot distinguish "never created"
  from "archived by the network". Consumers must treat it as "no readable vault
  at this id".
- **Not audited.** There has been no third-party security review.

---

## Resource usage

See **[docs/RESOURCE_COSTS.md](docs/RESOURCE_COSTS.md)** for the reasoning. The
design decisions that matter:

- Vault records are **single ledger entries**, so a mutator is one read and one
  write of one entry, plus the instance entry for the id counter.
- `get_vaults_by_owner` costs `limit` point reads, not a scan.
- `to_summary` exists so a page of 25 vaults never serialises 25 beneficiary
  vectors.
- Event payloads carry deltas, not reconstructed state.
- `Bps` arithmetic is integer-only; no floating point and no allocation.
- Creating a vault performs exactly three persistent writes (record, owner count,
  owner slot) plus one instance write (id counter).
- The release profile is tuned for `.wasm` size (deploy cost): `opt-level = "z"`,
  `lto`, `codegen-units = 1`, `panic = "abort"`, symbols stripped, with
  `overflow-checks = true` **retained** so arithmetic mistakes fail loudly rather
  than wrapping.

---

## Local development

Requires Rust ≥ 1.84 and the Soroban wasm target.

```bash
# Toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add wasm32v1-none

# Stellar CLI (v22 or newer)
cargo install --locked stellar-cli     # or download a release binary
```

## Testing

```bash
cargo test --workspace          # 123 unit tests + 9 external integration tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Test coverage by area (see `contracts/heirvault/src/test.rs`):

| Area | Representative cases |
|---|---|
| Vault creation | success, unique ids, every invalid period, non-token asset, contract-as-asset, no auth, unknown id |
| Deposits | real token movement, multiple deposits, zero/negative, wrong asset, no auth, after activation, after cancel, during grace |
| Beneficiaries | add/remove/update, 1 bps, 9 999 bps, exactly 100%, 101%, duplicates, owner/contract as heir, cap, slot retention, re-add, frozen after activation |
| Check-in | success, resets deadline, no auth, repeated, `deadline-1`, `deadline`, `deadline+1`, during grace, `grace_end-1`, `grace_end`, after `grace_end`, after activation, after cancel |
| Grace period | every boundary above, plus `get_deadlines` reporting exact values |
| Activation | too early, at deadline, during grace, at `grace_end`, permissionless, repeat, no beneficiaries, incomplete allocation, zero balance, cancelled, each of the three modes |
| Guardians | default threshold, duplicates, caps, owner/contract rejected, remove rules, threshold bounds, one-vote-per-attempt, unregistered, removed, no auth, mode mismatch, threshold reached, reset on check-in, anti-lockout, frozen after activation |
| Claims | before activation, single heir, proportional split, rounding dust, claim order independence, double claim (both before and after completion), unregistered, no auth, cancelled vault, position reporting, pagination |
| Cancellation | success, during grace, after grace expiry, after activation, repeat, no auth, partial/full withdrawal, over-withdrawal, before cancel, after activation, no auth |
| Pagination | page metadata, stable offsets, past-the-end, zero limit, clamped limit, independent beneficiary/guardian/claim paging |
| Authorization | a 12-entry-point matrix against a wrong-but-authorized caller, plus every entry point with no auth at all |
| Invariants | 300-step randomized allocation sequence, claimed ≤ deposited, activated never returns to active, cancelled never returns to active, completed vault is settled, approvals ≤ guardian count, internal balance ≡ tokens held |
| Events | each event asserted individually and a full-lifecycle sweep asserting all 14 names |

The `tests/` package adds 9 integration tests that consume the contract through
its public ABI only — no `pub(crate)` access — which is what proves the generated
client and re-exported types are usable from an SDK, indexer or frontend.

## Building

```bash
cargo build --workspace --release --target wasm32v1-none
# → target/wasm32v1-none/release/heirvault.wasm
```

## Deployment

```bash
stellar network add testnet \
  --rpc-url https://soroban-testnet.stellar.org \
  --network-passphrase "Test SDF Network ; September 2015"

stellar keys generate my-deployer --network testnet --fund

stellar contract deploy \
  --wasm target/wasm32v1-none/release/heirvault.wasm \
  --source my-deployer --network testnet
```

---

## Testnet deployment

**This contract is deployed and exercised on Stellar Testnet.** Full evidence,
including every transaction hash, is in **[docs/TESTNET.md](docs/TESTNET.md)**.

| Item | Value |
|---|---|
| Network | Stellar Testnet (protocol 29) |
| **HeirVault contract ID** | `CC6QRWVXN3V4UOADEZSGXNYRQMR252BXH6ERXOSENR64JTKIRCGA3ADO` |
| Wasm hash | rebuilt from this tree; current build is not byte-identical to the previously documented deployment |
| Explorer | https://stellar.expert/explorer/testnet/contract/CC6QRWVXN3V4UOADEZSGXNYRQMR252BXH6ERXOSENR64JTKIRCGA3ADO |
| Deployment tx | https://stellar.expert/explorer/testnet/tx/0c21a7a0af6f1586103060b36f0ad3aac17b9f187bf90d4049df6a3264224bed |
| Demo SEP-41 asset (SAC) | `CCQACXRCTDFOGOMP6NGJVGASIS3LG4B6WN4H74JNJTQFRBTBMQZ2SMGB` (`HVUSD`) |

Verified on-chain with real transactions:

- ✅ deployed the wasm and created the contract instance
- ✅ deployed a real SEP-41 asset (Stellar Asset Contract) and minted it
- ✅ `create_vault` (both `MissedCheckIn` and `GuardianApproval` modes)
- ✅ `add_beneficiary` (including a 3333/3333/3334 three-way split)
- ✅ `deposit` — real SEP-41 `transfer` from owner to contract, twice
- ✅ `check_in` on a live vault, with the new deadline read back
- ✅ `check_in` correctly rejected with `GracePeriodExpired` (#42) after the window
- ✅ `activate_vault` permissionlessly, called by an unrelated account
- ✅ `claim` by three heirs, exact distribution, contract drained to `0`
- ✅ `guardian_added`, `guardian_threshold_updated`, `guardian_approved`
- ✅ activation rejected with `NotEligible` (#34) at 1-of-2 approvals, accepted at 2-of-2
- ✅ `cancel_vault`, two partial `withdraw`s, over-withdrawal rejected with `InsufficientBalance` (#51)
- ✅ activation rejected with `VaultCancelled` (#35) on a cancelled vault
- ✅ `get_vaults_by_owner` pagination against live state

### Reproducing the Testnet walkthrough

```bash
export CID=CC6QRWVXN3V4UOADEZSGXNYRQMR252BXH6ERXOSENR64JTKIRCGA3ADO
export TOKEN=CCQACXRCTDFOGOMP6NGJVGASIS3LG4B6WN4H74JNJTQFRBTBMQZ2SMGB
export OWNER=$(stellar keys address heirvault-deployer)

# 1. Create a vault: 30-day check-in, 7-day grace.
stellar contract invoke --id $CID --source heirvault-deployer --network testnet -- \
  create_vault --owner $OWNER --asset $TOKEN \
  --activation_mode MissedCheckIn --check_in_period 2592000 --grace_period 604800

# 2. Name heirs (allocations must total 10000 bps before activation).
stellar contract invoke --id $CID --source heirvault-deployer --network testnet -- \
  add_beneficiary --vault_id 1 --beneficiary <HEIR_A> --allocation_bps 6000
stellar contract invoke --id $CID --source heirvault-deployer --network testnet -- \
  add_beneficiary --vault_id 1 --beneficiary <HEIR_B> --allocation_bps 4000

# 3. Fund it.
stellar contract invoke --id $CID --source heirvault-deployer --network testnet -- \
  deposit --vault_id 1 --asset $TOKEN --amount 1000000000

# 4. Keep it alive.
stellar contract invoke --id $CID --source heirvault-deployer --network testnet -- \
  check_in --vault_id 1

# 5. Miss the window, then anyone may release — 30d + 7d after the last check-in.
stellar contract invoke --id $CID --source heirvault-deployer --network testnet -- \
  activate_vault --vault_id 1

# 6. Each heir claims their own share.
stellar contract invoke --id $CID --source <heir-a-key> --network testnet -- \
  claim --vault_id 1 --beneficiary <HEIR_A>
```

---

## Contract invocation examples

```bash
# --- reads ---
stellar contract invoke --id $CID --network testnet -- get_vault --vault_id 1
stellar contract invoke --id $CID --network testnet -- get_vault_status --vault_id 1
stellar contract invoke --id $CID --network testnet -- get_deadlines --vault_id 1
stellar contract invoke --id $CID --network testnet -- is_claimable --vault_id 1
stellar contract invoke --id $CID --network testnet -- get_total_allocation --vault_id 1
stellar contract invoke --id $CID --network testnet -- get_guardian_status --vault_id 1
stellar contract invoke --id $CID --network testnet -- get_claim --vault_id 1 --beneficiary <HEIR>
stellar contract invoke --id $CID --network testnet -- get_vault_count
stellar contract invoke --id $CID --network testnet -- schema_version

# --- paged reads ---
stellar contract invoke --id $CID --network testnet -- \
  get_vaults_by_owner --owner $OWNER --offset 0 --limit 10
stellar contract invoke --id $CID --network testnet -- \
  get_beneficiaries --vault_id 1 --offset 0 --limit 10
stellar contract invoke --id $CID --network testnet -- \
  get_guardians --vault_id 1 --offset 0 --limit 10
stellar contract invoke --id $CID --network testnet -- \
  get_claims --vault_id 1 --offset 0 --limit 10

# --- guardian-gated vault ---
stellar contract invoke --id $CID --source $OWNER_KEY --network testnet -- \
  create_vault --owner $OWNER --asset $TOKEN \
  --activation_mode GuardianApproval --check_in_period 2592000 --grace_period 604800
stellar contract invoke --id $CID --source $OWNER_KEY --network testnet -- \
  add_guardian --vault_id 2 --guardian <G1>
stellar contract invoke --id $CID --source $OWNER_KEY --network testnet -- \
  set_guardian_threshold --vault_id 2 --threshold 2
stellar contract invoke --id $CID --source <g1-key> --network testnet -- \
  guardian_approve --vault_id 2 --guardian <G1>

# --- cancel and recover ---
stellar contract invoke --id $CID --source $OWNER_KEY --network testnet -- cancel_vault --vault_id 3
stellar contract invoke --id $CID --source $OWNER_KEY --network testnet -- \
  withdraw --vault_id 3 --amount 100000000
```

### Pagination convention

Every list endpoint returns `{ items, meta }` where `meta` is:

```jsonc
{ "total": 7, "offset": 0, "limit": 3, "has_more": true, "next_offset": 3 }
```

One paging loop works for every collection: call with `offset: 0`, then keep
calling with `meta.next_offset` until `has_more` is `false`. Offsets are stable
because the collections are append-only.

---

## Planned (not implemented)

These are **not** in the contract. They are listed so nothing is implied that has
not been built:

- **`ScheduledRelease` activation mode** — release at a fixed timestamp
  regardless of check-ins. The `ActivationMode` enum is designed to accept new
  variants, but adding one changes stored data and therefore needs a schema
  version bump and a migration.
- **Contract upgrades / admin role** — v1 is immutable. Adding an upgrade path
  requires deciding who controls it, which is a governance question rather than a
  technical one.
- **Multi-asset vaults** — one SEP-41 asset per vault today.
- **Beneficiary consent (accept/decline)** and **renunciation**.
- **`get_vaults_by_beneficiary`** — requires a second append-only index; it was
  left out rather than added speculatively, to avoid extra writes on the hot
  deposit/claim paths.
- **Keeper bounties** — paying whoever calls `activate_vault`. Would need a
  careful design so it cannot be gamed to drain a vault.
- **An on-chain audit trail of status transitions** — events cover it off-chain,
  but a stored history would allow on-chain queries.
- **Third-party security audit** and a formal verification pass.

---

## Remaining work

1. **Third-party security audit.** Nothing here has been reviewed by anyone but
   its author. This is the single biggest gap.
2. **Property-based / fuzz testing** with `cargo-fuzz` to complement the
   randomized-sequence invariant tests in the suite.
3. **Harden the asset probe.** `decimals()` is a good signal but not proof; a
   deeper SEP-41 conformance check would catch more bad assets at creation.
4. **Decide the upgrade/governance story** before any mainnet deployment.
5. **Wire up an indexer** that consumes the event stream, and build the HeirVault
   SDK on top of the generated client — the contract's paginated read surface and
   stable error/event ABI are designed for exactly that.
6. **Raise the production period floors.** The `60s` minimums exist for
   testability; a real deployment should enforce day-scale minimums.
7. **Trustline preflight.** The Testnet run showed claims reverting when an heir
   lacked an asset trustline. The contract is correct (the revert is atomic and
   the retry worked), but an SDK should preflight this so heirs get a clear
   message instead of a failed transaction.

---

## Acknowledgements

Architectural inspiration from the public
[`SoroWill/sorowill-contracts`](https://github.com/SoroWill/sorowill-contracts)
project, which demonstrated that on-chain inheritance on Soroban is practical.
HeirVault's data model, contract logic, state machine, storage layout, event
schema, activation modes, and test suite are original implementations.

## Licence

Apache-2.0.
