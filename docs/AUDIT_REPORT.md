# HeirVault audit and remediation report

**Scope:** every entry point and module in `contracts/heirvault/src/`
(`lib.rs`, `types.rs`, `storage.rs`, `events.rs`, `vault.rs`,
`beneficiaries.rs`, `guardians.rs`, `claims.rs`, `errors.rs`), the test modules
(`test.rs`, `property.rs`, `profile.rs`), the external integration suite in
`tests/`, and the documentation in `README.md` and `docs/`.
**Method:** line-by-line source review against the documented invariants, then
an executable check for every claim (build, tests, lints, and a live Soroban RPC
query of the deployed instance).
**Author:** automated review. **This is not an independent third-party audit**
and must not be described as one.

Toolchain used for all results below:

| Tool | Version |
|---|---|
| `rustc` / `cargo` | 1.99.0 |
| `soroban-sdk` | 22.0.11 |
| target | `wasm32v1-none` |
| RPC | `https://soroban-testnet.stellar.org` (latest ledger at time of check: 5,131,033) |

---

## 1. Issues found and fixed

### 1.1 Pagination cursor was derived from the returned count, not the scan position — **fixed**

**Where:** `storage::page_meta` and its callers `vault::get_by_owner`,
`vault::get_by_beneficiary` (plus the three non-skipping pages, which were only
coincidentally correct).

**Defect.** `get_by_owner` / `get_by_beneficiary` deliberately *skip* a slot whose
vault record is unreadable (archived) so one dead entry cannot fail a whole page.
But the `PageMeta` was built from the number of records **returned**:

```rust
let consumed = offset.saturating_add(returned);   // returned != slots scanned
let has_more = consumed < total;
next_offset: Some(consumed)
```

When any slot is skipped, `returned < slots scanned`, so `next_offset` points
**back into a region the scan already walked**. Concretely:

* A page that skipped a slot re-reads it on the next call (duplicate records).
* If **every** slot in the requested window is unreadable, the page returns zero
  records while `has_more` stays `true` and `next_offset == offset`. A naive
  "loop until `has_more == false`" client — the exact loop the README documents —
  never terminates.

The contract's own storage docs identify an unreadable index slot as an expected
condition ("one archived entry cannot make an owner's other vaults unlistable"),
so this path is reachable by design.

**Fix.** `page_meta` now takes the scan cursor (the index just past the last slot
inspected) and derives both fields from it:

```rust
pub fn page_meta(total: u32, offset: u32, limit: u32, cursor: u32) -> PageMeta {
    let has_more = cursor < total;
    PageMeta { total, offset, limit, has_more,
               next_offset: if has_more { Some(cursor) } else { None } }
}
```

Every caller passes its `cursor`. For pages that never skip, `cursor ==
offset + returned`, so behaviour is unchanged; for pages that skip, the cursor
now advances past the skipped slots. No API signature changed.

**Evidence.** Three regression tests were added and each **fails against the old
logic and passes against the new**:

| Test | Asserts |
|---|---|
| `paging_terminates_when_every_index_slot_is_unreadable` | a page whose only slot is unreadable returns `has_more == false`, `next_offset == None` (no infinite loop) |
| `paging_skips_an_unreadable_vault_without_repeating_a_record` | walking one record per page over a gap yields each readable vault exactly once |
| `beneficiary_paging_skips_an_unreadable_vault` | the beneficiary-side index advances past a skipped vault |

The tests simulate archival by deleting the index slot / vault record directly
through the host storage API inside `env.as_contract(...)`. Temporarily restoring
the old `page_meta` makes all three fail (verified); restoring the fix makes the
whole suite pass.

### 1.2 The documented wasm build command does not build — **fixed**

**Where:** `README.md` → *Building*.

**Defect.** The command `cargo build --workspace --release --target wasm32v1-none`
compiles the `tests/` workspace member, whose `soroban-sdk` dependency enables the
`testutils` feature. That feature is unsupported on the wasm target, so the build
fails with `error: 'testutils' feature is not supported on 'wasm' target` and no
artifact is produced.

**Fix.** The documented command is now package-scoped:

```bash
cargo build -p heirvault --release --target wasm32v1-none
# → target/wasm32v1-none/release/heirvault.wasm  (79,154 bytes)
```

**Evidence.** `cargo build --workspace ... --target wasm32v1-none` exits `101`
with no `.wasm`; `cargo build -p heirvault ...` exits `0` and produces the file.

### 1.3 Deployment evidence did not match the live contract — **fixed by redeploying (verified hash match)**

**Where:** `docs/TESTNET.md`, `README.md` → *Testnet deployment*.

**Defect.** The deployment record was internally contradictory: the artefact table
said the local build was "not byte-identical" to the deployment, while the
summary asserted "✅ wasm deployed; **local sha256 matches the on-chain install
hash**". The current source cannot reproduce the deployment, and one code example
in the walkthrough referenced the *old* contract id `CDVHAO…B4EZ` even though the
live contract is `CC6QRW…3ADO`.

**Verification (read-only, via Soroban RPC `getLedgerEntries`).** Fetching the
contract-instance ledger key and decoding the `ContractExecutable` gives:

| | Value |
|---|---|
| Contract id | `CC6QRWVXN3V4UOADEZSGXNYRQMR252BXH6ERXOSENR64JTKIRCGA3ADO` |
| On-chain `Wasm` executable hash | `66306452322d31042b7a2d300e98c8a0f494c2963330722a1ad1e16e3646d746` |
| Fresh build of this tree (`sha256`) | `b1b2a61c01e6d77ac0daf1c7150d8e87a6af92375ffdc803d54755ed71dc9816` |
| Deployed wasm size (from the record) | 76,552 bytes |
| Current-source wasm size | 79,154 bytes |

The two hashes differ, so **at the time of that check the live instance ran an
earlier revision of this contract, not the code in this tree.** The transaction
history in `TESTNET.md` is genuine and remains a valid record of that earlier
revision, but it was not evidence that the current source is deployed.

**Fix.** Both documents now state the mismatch explicitly, with the actual hashes,
and point to the (corrected) build/deploy steps. Documentation was changed so it
does not claim a correspondence that does not exist.

**Resolution (redeploy + re-verify).** The hardened contract was subsequently
redeployed to Testnet following the corrected steps, with a freshly funded
deployer key:

| | Value |
|---|---|
| New contract id | `CB4I2OSSNXQCH3DGHKGYMG5DPDOUDD55YG3SSDOK4JICVDH5B7KLEWI6` |
| Deploy transaction | `3333aa5a7a92f600b009f8c7a8c12f0c546dc59c995ed617dabfb30a0ca8d068` |
| On-chain `Wasm` executable hash (re-read via RPC) | `b1b2a61c01e6d77ac0daf1c7150d8e87a6af92375ffdc803d54755ed71dc9816` |
| Fresh build of this tree (`sha256`) | `b1b2a61c01e6d77ac0daf1c7150d8e87a6af92375ffdc803d54755ed71dc9816` — **match** |

The on-chain hash now equals a local build of this source. The full walkthrough
was re-run against the new instance and every transaction hash is recorded in
`docs/TESTNET.md`; all distribution and refusal invariants reproduced
(see that file's *Summary*).

### 1.4 Documentation facts that were stale — **fixed**

| Document | Claim | Correction |
|---|---|---|
| `README.md` (architecture + *Testing*) | "123 in-crate tests" / "123 unit tests" | 136 unit tests (125 at the start of this review) |
| `README.md` (*Building*) | workspace wasm build | package-scoped build (see §1.2) |
| `docs/STORAGE.md` | "the complete key space" listed 4 `DataKey` variants | added `BeneficiaryVaultCount` / `BeneficiaryVault` and their writers |
| `docs/STORAGE.md` | "The instance entry is refreshed from every mutating entry point" | only the six vault-lifecycle mutators call `touch_instance`; the config mutators do not (archived data is restorable, so this is not a fund risk) |
| `docs/STORAGE.md` | `create_vault` budget "4 ledger entries" | 5, as measured by `profile_entry_points` and shown in `RESOURCE_COSTS.md` |
| `docs/TESTNET.md` | old contract id in a `balance` invocation | current contract id |

### 1.5 Pre-existing formatting drift — **fixed**

`cargo fmt --all -- --check` failed on three pre-existing locations
(`storage.rs`, `test.rs`, `vault.rs`). `cargo fmt --all` was applied so the
documented check passes. (Some drift may be attributable to `rustfmt` version
differences between 1.99.0 and the toolchain used originally.)

---

## 2. Tests added

The suite went from **125 to 136 unit tests**; the 9 external integration tests
are unchanged. Everything runs in the ordinary `cargo test` suite — no nightly
toolchain and no separate fuzz crate is required.

### 2.1 Example-based and adversarial tests (`src/test.rs`)

| Test | Property |
|---|---|
| `paging_terminates_when_every_index_slot_is_unreadable` | skip-aware paging cannot loop forever |
| `paging_skips_an_unreadable_vault_without_repeating_a_record` | no record is repeated or lost across a gap |
| `beneficiary_paging_skips_an_unreadable_vault` | beneficiary index uses the same cursor |
| `a_failed_token_transfer_reverts_every_claim_side_effect` | checks → effects → interactions: a `transfer` that traps rolls back `claimed`, `balance`, `total_claimed`; a retry then settles exactly and a second claim is rejected |
| `an_heir_whose_share_rounds_to_zero_is_still_marked_claimed` | a 1-unit vault split 9999/1 pays `0` then `1`, reaches `COMPLETED`, drains to `0` |
| `a_reentrant_token_cannot_call_back_into_the_vault_during_transfer` | a hostile token that calls back into the vault from inside `transfer` cannot double-pay an heir or corrupt accounting |

The failure tests use purpose-built token contracts (also in `test.rs`) —
`FailingToken`, whose `transfer` traps on demand, and `ReentrantToken`, which
re-enters the vault — so the rollback and re-entrancy paths are *executed*
rather than asserted from the code's shape.

### 2.2 Re-entrancy: what the hostile token proved (`src/test.rs`)

`ReentrantToken` arms itself, and on the next `transfer` it disarms and makes two
call-backs into the vault: a read-only `get_vault` (unauthenticated, so nothing
in HeirVault could legitimately reject it) and `claim` for the heir being paid
(the call a missing guard would turn into a second payout). It records the
outcome of each separately, which distinguishes "the vault re-ran and its
`claimed` flag saved us" from "the call never reached the vault again".

Both call-backs come back as `InvokeError::Abort`. **The second is not a
HeirVault check** — it never re-enters the contract at all. Soroban runs every
contract-to-contract call with `ContractReentryMode::Prohibited`
(`soroban-env-host` 22.1.3: `src/host/frame.rs`, the re-entry distance check in
`call_n_internal`, reached from `Host::call`/`Host::try_call` via
`CallParams::default_external_call()`), so a contract already on the call stack
cannot be called again. The read-only probe is what pins this down: it touches no
state and needs no authorization, so the only thing that can refuse it is the
host rule.

So HeirVault's safety here rests on **two independent layers**: the host refuses
the call outright, and — separately — `claim` marks the slot before it transfers,
so even a host that permitted re-entry (or a future `Allowed`-mode path) would
find `AlreadyClaimed`. The test asserts both the refusal and the exact
distribution: one `inheritance_claimed` event, `total_claimed == deposit`,
`balance == 0`, `status == COMPLETED`.

### 2.3 Property-based (mini-fuzz) tests (`src/property.rs`)

A new module drives seeded random sequences over the four entry points the
project cares most about and asserts invariants that must hold for *every*
sequence. A deterministic xorshift generator is seeded per test, so a failure is
reproducible and reports the failing iteration.

| Test | Asserts over random sequences |
|---|---|
| `property_create_vault_period_range_is_exact` | a period is accepted **exactly** when it is inside the documented range (probed either side of both bounds), and `get_vault_count` matches the number actually created |
| `property_deposit_balance_equals_tokens_held` | the tracked balance always equals the tokens the contract actually holds, throughout a random deposit sequence; zero/negative amounts are always refused |
| `property_random_allocation_edits_never_exceed_one_hundred_percent` | 200 random add/remove/update operations never push the active allocation total above 10 000 bps |
| `property_random_lifecycle_distributes_the_deposit_exactly` | for 30 generated vaults (1–10 heirs, allocations summing to exactly 100%, random deposit, random claim order): `sum(payouts) == deposit`, no negative payout, balance drains to `0`, no dust is left in the token contract, status is `COMPLETED`, and no heir can claim twice |
| `property_random_withdrawals_never_exceed_the_balance` | a withdrawal is accepted exactly when it is positive and within the remaining balance, and internal balances always equal the tokens held |

Iteration counts default to values that keep `cargo test --workspace` quick and
are overridable for a longer soak:
`HEIRVAULT_PROPTEST_ITERS=5000 cargo test -p heirvault --lib property`.

**Evidence that these tests can fail.** The distribution invariant was checked
against an injected defect: changing `claim` so the final heir *no longer*
absorbs the rounding remainder (i.e. dropping the `others_outstanding == 0 →
vault.balance` branch) makes
`property_random_lifecycle_distributes_the_deposit_exactly` fail on iteration 0
with `left: 574410451, right: 574410455` — a 4-unit rounding defect a
hand-written example would plausibly have missed. The mutation was reverted
immediately, and the file was diffed against `HEAD` afterwards to confirm no
mutation remained.

---

## 3. Reviewed and found correct (no change)

These were checked against the source and, where applicable, against tests; each
held.

* **Authorization.** Every privileged entry point calls `require_auth()` on an
  address read from state, never on a caller-supplied argument. `create_vault`
  (owner), `deposit` / beneficiary edits / guardian config / `check_in` /
  `cancel_vault` / `withdraw` (vault owner), `guardian_approve` (guardian),
  `claim` (beneficiary). `activate_vault` is intentionally permissionless and
  transfers nothing; it re-derives every condition from state and the ledger
  clock, so the worst case is activating an already-due vault.
* **Asset movement.** `deposit` compares the supplied asset to the configured one
  *before* transferring and performs a real SEP-41 `transfer` that reverts on
  failure. `withdraw` and `claim` transfer out only to the owner / a registered
  active beneficiary. No path moves funds on `activate_vault`.
* **State machine.** `ACTIVE`/`GRACE_PERIOD` are the only editable, deposit-taking
  states; `ACTIVATED` freezes configuration and blocks cancel/withdraw/check-in;
  `CANCELLED` permits only withdrawal; `COMPLETED` is inert. Time-derived
  `ACTIVE → GRACE_PERIOD` uses half-open intervals and the ledger clock only.
* **Allocation.** `sum(active allocations) <= 10_000` on every mutator;
  `== 10_000` enforced at activation. Zero/`>10_000` allocations rejected;
  duplicates rejected; owner and contract addresses rejected; lists capped at 10
  beneficiaries / 5 guardians, so every linear scan is bounded.
* **Claims.** Entitlements use the `distribution_balance` snapshot, never the live
  balance. The `claimed` flag is set before the transfer, so no address can be
  paid twice (an address can appear in at most one slot because re-adding
  reactivates the retained slot). The last heir absorbs rounding dust, so
  `sum(payouts) == distribution_balance` exactly and the vault drains to `0`.
  A payout is refused if it would exceed the remaining balance.
* **Guardians.** Approvals are per-guardian flags, recomputed from slots (never
  blindly incremented), one per attempt, cleared by every owner check-in.
  Removing the last guardian of a guardian-gated vault, or a zero/over-range
  threshold, is refused (anti-lockout). Approvals are rejected on modes that do
  not consult guardians.
* **Arithmetic.** All value arithmetic is checked; `overflow-checks = true` is
  retained in the release profile. No `unwrap()` on caller input; the one
  `expect` is an unreachable count overflow.
* **Re-entrancy / call-back safety.** The contract performs external calls in
  exactly three places: the SEP-41 `transfer` in `deposit`, `withdraw` and
  `claim`, and the `decimals` probe in `create_vault`. In all of them state is
  written before the call and every post-call value is read from the record
  loaded *before* it, so a call-back that somehow succeeded would observe
  already-updated state. Independently, the host refuses the call-back entirely
  (see §2.2). `claim` is the only entry point that pays a third party, and its
  `claimed` flag is set before the transfer, so the "paid twice" path is closed
  by ordering as well as by the host.
* **Events / errors.** Every mutator emits exactly one documented event (claims
  emit two on the final call, documented), topic `[0]` is a stable name and
  `[1]` the vault id; error codes are stable and the ABI is append-only.

---

## 4. Verification performed

| Check | Command | Result |
|---|---|---|
| Unit tests | `cargo test -p heirvault --lib` | **136 passed; 0 failed** (includes the 5 property tests and the re-entrancy test) |
| Integration tests | `cargo test --workspace` | **9 passed; 0 failed** |
| Property suite catches a real defect | mutate `claim` to drop the rounding remainder, then `cargo test -p heirvault --lib property` | **FAILS on iteration 0** as intended; mutation reverted and diffed against `HEAD` |
| Re-entrancy refusal | `cargo test -p heirvault --lib reentrant` | **1 passed** — both call-backs recorded as refused |
| Lints | `cargo clippy --workspace --all-targets -- -D warnings` | **clean** |
| Formatting | `cargo fmt --all -- --check` | **clean** |
| Release wasm | `cargo build -p heirvault --release --target wasm32v1-none` | **exit 0**, 79,154 bytes, sha256 `b1b2a61c…16` |
| Deployed wasm hash | Soroban RPC `getLedgerEntries` on the contract-instance key | `b1b2a61c…dc9816` (**= current source**) |
| Live walkthrough | `stellar contract invoke` against `CB4I2…LEWI6` | all flows reproduced; hashes in `docs/TESTNET.md` |
| Dependency audit | `cargo audit` (RustSec, 1,296 advisories) | **no vulnerabilities**; 2 `unmaintained` warnings (`derivative` 2.2.0, `paste` 1.0.15) inside the `soroban-sdk` macro dependency tree, not first-party code |

Reproduce each with the commands above; the tools are the standard Rust toolchain
and `curl` against the public Testnet RPC. The `test_snapshots/` directory the
Soroban test host writes is git-ignored and is not part of the contract.

---

## 5. Known limitations of this review

1. **Not an independent audit.** No third party reviewed this code. This document
   describes an automated review by the same process that wrote the changes.
2. **Testnet is not mainnet.** The redeployed instance matches this source
   (§1.3), but Testnet has no economic weight and its history is not
   production evidence.
3. **No coverage-guided fuzzing or formal verification.** Coverage is
   example-based plus the seeded, in-suite property tests added in §2.3 — those
   explore generated sequences but do not shrink counterexamples or evolve inputs
   the way `cargo-fuzz` would, and there are no proofs.
4. **No live mainnet-cost measurement.** The resource figures are from the Soroban
   test host and exclude VM instantiation and transaction size.
5. **`--workspace` wasm builds fails by design.** The `tests` crate is a workspace
   member that enables `testutils`; only `-p heirvault` produces a deployable
   wasm. This is now documented rather than silently worked around.
6. **Timestamp trust and owner-key compromise** remain out of scope, as stated in
   `docs/SECURITY.md`.
