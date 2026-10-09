# Resource usage

Soroban bills four things per invocation: **CPU instructions**, **ledger entries
read**, **ledger entries written**, and **bytes** read/written (plus rent and
event size). This document reports what HeirVault actually costs, explains the
decisions behind those numbers, and lists the guard rails that stop them from
silently regressing.

## How to reproduce

```bash
cargo test -p heirvault --lib profile_entry_points -- --nocapture
```

The test invokes every entry point once against a realistic fixture and prints a
table; it also asserts the properties the numbers are supposed to demonstrate, so
a regression fails the build rather than quietly reporting worse numbers.

## Measured profile

Soroban test host, protocol 22, `soroban-sdk` 22.0.11. `cpu` is modelled
instructions; `rd`/`wr` are ledger entries; `bytes` are ledger bytes billed.

| entry point | cpu | rd ent | wr ent | rd bytes | wr bytes | event bytes |
|---|---:|---:|---:|---:|---:|---:|
| `create_vault` | 183,774 | 3 | 5 | 624 | 1,400 | 252 |
| `add_beneficiary` (1st) | 139,076 | 2 | 2 | 1,040 | 1,152 | 160 |
| `add_beneficiary` (10th) | 260,793 | 2 | 2 | 2,768 | 2,880 | 160 |
| `update_beneficiary` | 148,656 | 2 | 2 | 1,232 | 1,152 | 168 |
| `remove_beneficiary` | 147,690 | 2 | 2 | 1,232 | 1,152 | 152 |
| `deposit` | 282,490 | 5 | 4 | 2,068 | 1,600 | 412 |
| `set_guardian_threshold` | 151,221 | 2 | 2 | 1,464 | 1,384 | 128 |
| `guardian_approve` | 157,097 | 2 | 2 | 1,464 | 1,384 | 160 |
| `check_in` | 145,824 | 3 | 2 | 1,284 | 1,152 | 168 |
| `activate_vault` | 138,059 | 2 | 1 | 1,284 | 1,084 | 176 |
| `activate_vault` (guardian) | 150,127 | 2 | 1 | 1,516 | 1,316 | 176 |
| `claim` (of 2) | 309,200 | 5 | 4 | 2,264 | 1,796 | 452 |
| `claim` (last) | 318,850 | 5 | 4 | 2,264 | 1,796 | 592 |
| `cancel_vault` | 146,149 | 3 | 2 | 1,284 | 1,156 | 160 |
| `withdraw` | 286,542 | 4 | 4 | 2,204 | 1,604 | 412 |
| `get_vault` (read) | 87,269 | 2 | 0 | 1,232 | 0 | 0 |
| `get_beneficiaries` (read) | 65,727 | 2 | 0 | 1,232 | 0 | 0 |
| `get_vaults_by_owner` (read) | 96,117 | 4 | 0 | 1,520 | 0 | 0 |

> These figures meter the *last top-level invocation* on the test host, which
> runs the contract directly rather than through a deployed wasm. VM
> instantiation, wasm entry reads and transaction size are therefore **not**
> included and real production costs are higher. Use them to compare entry
> points and to catch regressions, not as an absolute fee.

### What the numbers show

- **Token-free mutators (2 writes, 1–3 reads).** `check_in`, the beneficiary
  operations, the guardian operations, `activate_vault` and `cancel_vault` write
  the vault record plus at most the instance entry's TTL bump. Nothing scales
  with the number of vaults in existence.
- **`activate_vault` is the cheapest mutator (1 write, 138k CPU).** It writes one
  entry and emits one event; it does not touch the token. This matters: activation
  is the one entry point anyone can call, so keeping it cheap keeps the
  permissionless keeper path viable.
- **Value-moving entry points cost ~2× (4 writes, ~280–320k CPU).** `deposit`,
  `withdraw` and `claim` each perform a real SEP-41 `transfer`, which writes the
  token contract's own balance entries inside the same invocation. That cost is
  inherent to moving real assets; an entry point that did not pay it would not be
  moving anything.
- **Beneficiary count changes entry *size*, not entry *count*.** The tenth
  beneficiary costs the same **2 ledger writes** as the first — the difference is
  bytes (1,152 → 2,880) because the single vault record is larger. That is the
  intended shape: a bounded list inside one record keeps writes flat and reads
  proportional to the record, with no per-item ledger entry.
- **Claims do not get more expensive as they progress.** `claim (of 2)` and
  `claim (last)` both write 4 entries. A claim is a per-heir operation, so its
  cost is flat across the distribution — the last heir's extra event bytes
  (`vault_completed`) are the only difference.
- **Reads never write.** `get_vault`, `get_beneficiaries` and
  `get_vaults_by_owner` all report 0 write entries, which is what makes polling
  them from an indexer or frontend free of state-mutation cost.
- **Paging is O(limit), not O(total).** `get_vaults_by_owner` with `limit 1`
  reads 4 entries: the snapshot/instance entry, the owner count, one owner slot
  and the vault record. Asking for 10 vaults costs 4 + 9 more slot reads — it
  never depends on how many vaults exist globally.

## Optimisation decisions

Each of these was a deliberate choice, and each is verifiable in the table above.

### 1. One vault record, bounded lists inside it

Beneficiaries and guardians are vectors *inside* the vault record rather than
separate ledger entries. Consequences:

- **Writes are constant.** Adding a beneficiary is one record write, not a record
  write plus a new entry plus an index update. The `1st` vs `10th` comparison
  above proves the write count does not move.
- **A configuration change is one read and one write.** Enforcing the allocation
  invariant needs the whole list anyway, so a split layout would mean reading N
  entries to compute a sum and then writing one.
- **The trade-off is bytes.** The record grows linearly with the list — 1,040
  bytes read for one beneficiary, 2,768 for ten. This is acceptable only because
  the list is capped, which is why `MAX_BENEFICIARIES` is a *safety* constant, not
  a product limit. Without the cap, a vault could grow until its own activation
  no longer fit in a transaction's budget and its funds would be stranded.

### 2. An explicit owner index instead of a scan

`get_vaults_by_owner` does not iterate the contract's vaults. `create_vault`
appends one `OwnerVault(owner, i)` slot and bumps `OwnerVaultCount(owner)`, and a
page is `limit` point reads. The alternative — a vector of vault ids per owner,
or a full scan — is either an O(n) write on every creation or an O(total vaults)
read on every query. The index costs one extra persistent write per vault
creation (5 writes instead of 4) and makes every subsequent read independent of
global state.

### 3. A summary projection for list endpoints

`get_vaults_by_owner` returns `VaultSummary`, not `Vault`. A page of 25 vaults
therefore never serialises 25 beneficiary vectors and 25 guardian vectors. This
is the single largest byte saving on the read path, and it is why the paged read
stays at 1,520 bytes for a one-item page.

### 4. Events carry deltas, not reconstructed state

Payloads carry identifiers and the values that changed (`amount`, `new balance`,
`new total bps`) rather than a serialised copy of the vault. Event bytes are
billed as ledger writes, and a payload containing the whole vault would grow with
the beneficiary list on every event. Today the largest payload is a claim
(592 bytes, because it carries four values plus the timestamp).

### 5. Integer basis points, no floating point

Allocation is `u32` basis points and all share arithmetic is `i128`
multiply/divide. There is no decimal type, no rounding mode, and no allocation —
which is both a correctness property (the distribution sums exactly) and a CPU
one.

### 6. Reads stay writes-free

`get_vault` and friends refresh the TTL of the entries they touch, but they never
write state, so a polling client cannot change ledger state or pay write costs.
`get_vaults_by_owner` skips an unreadable slot rather than failing the page, which
also avoids any write on a partially stale index.

### 7. The release profile is tuned for deploy size, not for the meter

`opt-level = "z"`, `lto = true`, `codegen-units = 1`, `panic = "abort"`, symbols
stripped. The deployed wasm is **76,552 bytes**, and deploy cost is charged per
byte, so size matters more than a few percent of runtime instructions.

`overflow-checks` is deliberately **left enabled**. It costs a handful of
instructions on paths that already use explicit checked arithmetic, and in
exchange a mistake becomes a loud failure instead of a silent wrap that
mis-accounts somebody's inheritance. That is the right trade for this domain.

## Guard rails

The profile test asserts, and will fail the build if violated:

| Property | Assertion |
|---|---|
| Token-free mutators write a constant number of entries | `write_entries <= 2` for `check_in`, all beneficiary ops, all guardian ops, `activate_vault`, `cancel_vault` |
| Value-moving entry points stay within a constant budget | `write_entries <= 5` for `deposit`, `withdraw`, `claim` |
| Adding the tenth beneficiary costs no more writes than the first | `write_entries` equal between the two |
| Adding the tenth beneficiary adds no extra ledger *reads* beyond its own larger record | `read_entries(10th) <= read_entries(1st) + 1` |
| Paging stays O(limit) | `read_entries <= 4` for a one-item page |
| A claim's cost does not depend on being the final one | `write_entries` equal between `claim (of 2)` and `claim (last)` |
| Reads never write | `write_entries == 0` for all three read entry points |

## Optimisation debt

Honest notes on what was **not** optimised, and why:

- **No caching between calls.** Soroban has no memory that survives a call
  boundary, and an in-contract memo would be a second source of truth to keep
  consistent with the record. The design instead keeps the live state in one
  entry so there is nothing to reconcile.
- **Beneficiary lookups are linear scans.** Bounded by 10, so at most ten
  comparisons — an index would be more code and more writes for no measurable
  gain at this size.
- **`total_allocation_bps()` recomputes the sum on every mutator.** Bounded by 10
  additions. A cached total would be a derived field that could drift out of sync
  with the list, which is a worse failure than ten additions.
- **`get_vaults_by_owner` does not write back TTL extensions for entries it only
  observes through the index.** The vault record itself is always TTL-refreshed
  when loaded; the index slots are refreshed when written.
- **`VaultSummary` duplicates a handful of scalar fields.** It costs bytes on the
  read path to avoid serialising nested vectors, which is a clear win at any list
  size above one.
