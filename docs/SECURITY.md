# Security model

This document states what HeirVault protects against, how, and what it
explicitly does **not** protect against. It is written to be checkable against
the code rather than reassuring.

> **This contract has not been audited.** Everything below describes controls the
> author implemented and tested. It is not a substitute for an independent
> review, and should not be treated as one.

---

## Trust model

| Party | Trusted for | Not trusted for |
|---|---|---|
| **Vault owner** | configuring their own vault, check-in liveness | anything on another owner's vault |
| **Beneficiaries** | claiming only their own allocation | anything else |
| **Guardians** | voting once on an activation attempt | moving funds, changing configuration |
| **Anyone (keepers)** | calling `activate_vault` when conditions hold | influencing conditions or outcomes |
| **The ledger clock** | the definition of "now" | nothing else |
| **The asset contract** | honouring SEP-41 `transfer` | correct behaviour on failure — assumed to revert |

HeirVault is trust-minimised *between* parties. It necessarily trusts the Stellar
network for consensus time and the SEP-41 contract for token movement.

---

## Authorization

The rule applied consistently: **`require_auth()` is always called on an address
read from state, never on a caller-supplied parameter.** A caller cannot nominate
themselves as the owner by passing an argument, because there is no argument to
pass — the owner is looked up in the vault record.

| Entry point | Requires | Notes |
|---|---|---|
| `create_vault` | `owner` | caller picks their own address; must authenticate it |
| `deposit` | vault owner | only the owner funds their vault |
| `add_beneficiary` / `remove_beneficiary` / `update_beneficiary` | vault owner | |
| `add_guardian` / `remove_guardian` / `set_guardian_threshold` | vault owner | |
| `guardian_approve` | the named guardian | must be an *active* guardian |
| `check_in` | vault owner | the whole point: nobody else may postpone an inheritance |
| `cancel_vault` / `withdraw` | vault owner | |
| `claim` | the named beneficiary | must be an *active* beneficiary |
| `activate_vault` | **nobody** | permissionless by design — see below |

### Why `activate_vault` is permissionless

Because the premise is that the owner is gone. Requiring the owner to call it
would be self-defeating; requiring a named keeper would introduce a liveness
dependency (what if the keeper disappears too?). Permission is safe here because
the function:

- re-derives **every** condition from on-chain state and `env.ledger().timestamp()`,
  so it cannot be talked into releasing a vault that is not due;
- **transfers nothing** — it only flips state to open the claim phase;
- cannot be called twice (the second call returns `AlreadyActivated`).

Worst case, a keeper activates a vault that was already due. That is the intended
behaviour.

### How authorization is tested

Two complementary tests, because they prove different things:

1. **`every_privileged_call_fails_without_any_authorization`** — calls all 12
   privileged entry points with `env.set_auths(&[])`, disabling mock auth
   entirely. This proves an authorization check exists on each path.
2. **`privileged_entry_points_reject_the_wrong_authorizer`** — installs a
   *valid* authorization entry for a **different** address (an attacker) and
   asserts each call still fails. This is the stronger test: if a function
   required the attacker's auth instead of the owner's, case 1 would pass and
   case 2 would fail.

`claim` and `withdraw` require different authorizers (the heir, not the owner), so
they have dedicated tests of the same shape:
`claim_requires_the_beneficiary_to_authenticate`,
`guardian_approval_requires_the_guardian_to_authenticate`,
`withdraw_requires_owner_authentication`.

---

## Threat table

| Threat | Control | Test |
|---|---|---|
| Non-owner modifies beneficiaries | owner read from state + `require_auth` | wrong-authorizer matrix |
| Non-owner modifies guardians | same | wrong-authorizer matrix |
| Third party resets the check-in timer | `check_in` requires owner auth | `check_in_requires_owner_authentication` |
| Guardian approves twice to fake a quorum | per-guardian flag; count recomputed from slots | `a_guardian_can_approve_once_per_attempt`, `invariant_guardian_approvals_never_exceed_the_guardian_count` |
| Stale guardian votes replayed after recovery | check-in clears every flag and the counter | `a_check_in_clears_every_guardian_approval` |
| Guardian votes on a vault that ignores them | `ApprovalNotEnabled` | `approvals_are_rejected_when_the_mode_does_not_use_guardians` |
| Unregistered address claims | membership check against the slot list | `unregistered_addresses_cannot_claim` |
| Beneficiary claims twice | `claimed` flag set before transfer | `double_claim_is_rejected`, `double_claim_is_rejected_while_other_heirs_remain` |
| Claim before activation | status check | `claim_before_activation_is_rejected` |
| Claim after settlement | `VaultCompleted` | `invariant_a_completed_vault_is_fully_settled` |
| Allocation exceeds 100% | checked sum on every mutator | `allocation_may_not_exceed_one_hundred_percent`, 300-step randomized invariant test |
| Activation with an incomplete allocation | `== 10_000` enforced at activation | `activation_requires_a_full_one_hundred_percent_allocation` |
| Duplicate beneficiaries / guardians | explicit duplicate rejection | `add_beneficiary_rejects_duplicate_addresses`, `duplicate_guardians_are_rejected` |
| Zero or negative deposit | `InvalidAmount` | `deposit_rejects_non_positive_amounts` |
| Depositing the wrong token | asset equality checked *before* transfer | `deposit_rejects_mismatched_asset` |
| Bad asset contract at creation | `decimals()` probe + self-address rejection | `create_vault_rejects_asset_that_is_not_a_token` |
| Owner or contract address named as heir/guardian | `InvalidAddress` | `add_beneficiary_rejects_the_owner_and_the_contract` |
| Threshold above guardian count | `InvalidThreshold` | `threshold_must_be_positive_and_within_the_guardian_count` |
| Removing the last guardian of a gated vault (permanent lock) | anti-lockout rule | `the_last_guardian_of_a_guardian_gated_vault_cannot_be_removed` |
| Withdrawing more than the balance | balance check + checked subtraction | `withdraw_cannot_exceed_the_balance` |
| Cancellation interrupting a claim | cancel requires a non-activated state | `cancel_is_rejected_after_activation` |
| Claiming from a cancelled vault | status check | `a_cancelled_vault_cannot_be_claimed_from` |
| Timestamp boundary errors | half-open intervals, defined once | the check-in/grace boundary suite |
| Arithmetic overflow | checked arithmetic throughout, `overflow-checks` on | `ArithmeticOverflow` paths |
| Unbounded loops / reads | `MAX_BENEFICIARIES` = 10, `MAX_GUARDIANS` = 5, `MAX_PAGE_LIMIT` = 25 | cap tests, paging tests |
| Internal balance drifting from real tokens | effects-before-interactions + real transfers | `internal_balance_always_matches_the_tokens_held` |
| Rounding dust permanently trapped | remainder paid to the last claimant | `rounding_dust_goes_to_the_last_heir_to_claim` |

---

## Re-entrancy

Soroban has no re-entrancy protection primitive, so the discipline has to be
structural. HeirVault follows **checks → effects → interactions** everywhere a
token is involved:

```
1. validate every precondition
2. write the new state (balance, claimed flags, status, totals)
3. only then call token::transfer
```

Consequences:

- A hostile or buggy token that re-enters observes **post-transfer accounting**.
  On `claim`, the heir's `claimed` flag is already `true` and their payout is
  already deducted, so a re-entrant `claim` is rejected with `AlreadyClaimed`.
- If the transfer fails, the whole transaction reverts — including the state
  written in step 2. The internal balance cannot drift from the tokens actually
  held. **This was observed on Testnet**: two claims reverted at the token layer
  because the heirs lacked a trustline, and the later retries produced exactly
  the correct distribution, proving the revert left no residue.

There is no `reentrancy guard` storage cell, and deliberately so: adding one
would be a second piece of mutable state to keep consistent, and the
effects-before-interactions ordering already provides the property.

---

## Value conservation

Two invariants are enforced and tested:

```text
1.  balance == distribution_balance - total_claimed     (once activated)
2.  sum(all payouts) == distribution_balance            (at completion)
```

Invariant 2 holds *exactly*, not approximately, because the last heir to claim
receives the remaining balance rather than its floored share. Tested with a
three-way 3333/3333/3334 split of an odd amount both in the unit suite and in a
real Testnet run.

Additionally, `claim` refuses a payout that would exceed the remaining balance
(`InsufficientBalance`). This is the last line of defence: even if accounting
were somehow corrupted, a claim cannot overdraw a vault.

---

## Input validation summary

| Input | Rule |
|---|---|
| `check_in_period` | `60s ≤ p ≤ 5 years`, else `InvalidPeriod` |
| `grace_period` | `60s ≤ p ≤ 1 year`, else `InvalidPeriod` |
| `asset` | not the contract itself; answers `decimals()`, else `InvalidAsset` |
| deposit `amount` | `> 0`, else `InvalidAmount` |
| `allocation_bps` | `1 ≤ bps ≤ 10_000`, else `InvalidAllocation` |
| allocation total | `≤ 10_000`, else `AllocationExceeded` |
| beneficiary/guardian address | `≠ owner`, `≠ contract`, else `InvalidAddress` |
| guardian `threshold` | `1 ≤ t ≤ active guardians`, else `InvalidThreshold` |
| page `limit` | `> 0`; values above 25 are clamped, `0` is `InvalidPageLimit` |

The **minimum periods of 60 seconds** exist so the contract is testable. A
production deployment should raise them to day-scale minimums; the constants are
in one place (`types.rs`) for that reason.

---

## Known limitations

These are real properties of the design, documented rather than papered over.

1. **Ledger time, not wall time.** Deadlines use
   `env.ledger().timestamp()`, the validator-set consensus time. A contract
   cannot do better than the chain's own clock. On Stellar this tracks wall-clock
   closely; the mitigation is consensus itself, not contract code.

2. **An alive owner can always pre-empt activation.** An owner holding the key
   can `cancel_vault` any time before somebody activates the vault, including
   after the grace period expires. This is intentional — an authenticated owner
   is demonstrably alive, and paying a living person's estate would be the worse
   error — but it means heirs have no *guaranteed* entitlement while the owner
   can still sign. Users who want a hard deadline should consider a
   guardian-gated mode, where the release decision does not depend on the owner's
   continued cooperation.

3. **No beneficiary consent.** Naming an heir is unilateral. An heir cannot be
   forced to accept a transfer, but neither can they block one.

4. **`VaultNotFound` is ambiguous.** Soroban does not report *why* a persistent
   key is missing, so "never created" and "archived by the network" are
   indistinguishable. Consumers must not treat the error as proof of
   non-existence.

5. **The asset probe is best-effort.** `decimals()` proves a contract responds to
   one SEP-41 call, not that it implements the whole interface. The binding check
   is that `deposit` performs a real transfer which reverts on failure — so a
   hostile asset can never create phantom accounting, but it *can* be registered
   by an owner who then cannot fund it. This is self-inflicted.

6. **Owner-key compromise is out of scope.** An attacker with the owner's key can
   cancel and drain the vault. Inheritance contracts cannot defend against a
   stolen key; use a hardware signer or a multisig as the owner.

7. **No time-lock on cancellation.** There is no cool-down between `cancel_vault`
   and `withdraw`. An owner whose key is briefly compromised cannot recover the
   funds by racing the attacker.

8. **Single asset per vault, capped lists.** 10 beneficiaries, 5 guardians, one
   SEP-41 asset.

9. **Not audited, not formally verified.** No third-party review; no fuzzing
   beyond the randomized invariant tests in the suite.

---

## Pre-deployment checklist

Before any mainnet deployment:

- [ ] independent security audit
- [ ] raise `MIN_CHECK_IN_PERIOD` / `MIN_GRACE_PERIOD` to day-scale values
- [ ] decide the upgrade/governance story (v1 is immutable)
- [ ] `cargo-fuzz` targets for `create_vault`, `deposit`, `add_beneficiary`,
      `claim`
- [ ] SDK-side trustline preflight so an heir without a trustline gets a clear
      error instead of a reverted transaction
- [ ] run the full Testnet walkthrough at production period values
- [ ] document the operational model for keepers who call `activate_vault`
