# Storage architecture

This document is the reference for HeirVault's ledger layout: what is stored,
where, why each choice was made, and how the layout can evolve without breaking
existing vaults.

---

## The complete key space

Every ledger entry the contract writes is keyed by one variant of a single enum
(`storage::DataKey`). Making it a single enum is deliberate: introducing a new
namespace requires editing one type, so the whole layout is discoverable from one
place instead of being scattered across string literals.

```rust
#[contracttype]
pub enum DataKey {
    NextVaultId,                 // instance
    Vault(u64),                  // persistent
    OwnerVaultCount(Address),    // persistent
    OwnerVault(Address, u32),    // persistent
}
```

| Key | Durability | Written by | Read by | Purpose |
|---|---|---|---|---|
| `NextVaultId` | instance | `create_vault` | `create_vault`, `get_vault_count` | monotonic id allocator |
| `Vault(id)` | persistent | every mutator | every entry point touching a vault | the vault record |
| `OwnerVaultCount(owner)` | persistent | `create_vault` | `get_vaults_by_owner` | size of the owner's list |
| `OwnerVault(owner, i)` | persistent | `create_vault` | `get_vaults_by_owner` | slot `i` of the list |

There is **no temporary storage**. Every value a vault depends on has to outlive
a single transaction; a vault whose configuration expired between two calls
could become permanently unreadable, which for an inheritance instrument means
permanently lost funds.

---

## Durability choices

### Instance storage: the id counter

`NextVaultId` lives in instance storage because it is one small value that almost
every mutating entry point touches, and instance storage is a single ledger entry
that Soroban holds resident for the duration of a call. Putting it alongside the
contract instance means reading and bumping it costs one entry, not a
persistent-entry lookup plus TTL bookkeeping.

Vault ids start at **1** so that `0` is unambiguously "no vault" in client code.
Ids are allocated by incrementing with checked arithmetic and returning
`ArithmeticOverflow` rather than wrapping — an id can never be handed out twice.

### Persistent storage: vault records and the owner index

Vaults are persistent so that **each carries its own rent and TTL**. A single
instance entry would mean one owner's idle vault and another owner's active one
share a fate; with per-entry persistence, an abandoned vault expiring cannot take
down an active one.

The owner index is also persistent, for the same reason: it is per-owner data.

---

## Why an explicit owner index instead of a scanned collection

`get_vaults_by_owner` must not scan every vault in the contract. Instead:

- `create_vault` appends **one** slot: `OwnerVault(owner, i) -> vault_id`, plus
  one bump of `OwnerVaultCount(owner)`.
- A page is then `limit` point reads plus one count read.

The cost of a list call is therefore independent of how many vaults exist
globally, and independent of how many vaults the owner has. This is the
difference between an O(1)-per-record index and an O(total-vaults) scan.

**The index is append-only.** Vaults are never deleted, so slots are never
removed and offsets never shift underneath a client. That is what lets an SDK
page through an owner's vaults with a simple `offset += limit` loop without any
cursor invalidation logic.

Beneficiary and guardian lists are *not* indexed this way. They live inside the
vault record because they are bounded by `MAX_BENEFICIARIES` (10) and
`MAX_GUARDIANS` (5) — a linear scan over them is at most ten comparisons, and
keeping them in the record means a configuration change is one read and one
write rather than one read plus N writes.

---

## The versioned pool: `#![no_std]` and the release profile

The wasm build uses `panic = "abort"`, `lto`, `codegen-units = 1`,
`opt-level = "z"` and stripped symbols to minimise the deployed bytecode (deploy
cost is charged per byte). `overflow-checks` is deliberately **left on**: an
arithmetic mistake should trap loudly rather than wrap silently and mis-account
someone's inheritance. The cost is a few instructions on paths that already use
explicit checked arithmetic.

---

## TTL policy

Persistent entries expire unless their time-to-live is extended.

| Constant | Value | Meaning |
|---|---|---|
| `PERSISTENT_TTL_THRESHOLD` | 30 days (in ledgers) | extend once the entry drops below this |
| `PERSISTENT_TTL_EXTEND_TO` | 90 days (in ledgers) | extend out to this |

Both the read path (`load_vault`, `load_vault_opt`) and the write path
(`save_vault`) refresh the TTL. The practical effect:

> An owner who checks in at least once every ~30 days will never see a vault
> entry archived, no matter how long the vault lives.

The instance entry is refreshed from every mutating entry point
(`storage::touch_instance`), so the id counter can never be archived out from
under the contract.

The window is deliberately below Soroban's maximum TTL. There is a real
trade-off: a long TTL costs rent, and a too-short one risks a vault being
archived between a check-in deadline and an activation. 30/90 days sits
comfortably on the safe side for a vault whose *whole point* is to be recovered
after a long silence.

---

## Schema versioning

Two independent markers exist, and they are not the same thing:

| Marker | Where | Meaning |
|---|---|---|
| `types::SCHEMA_VERSION` | compiled constant | the layout the **running code** expects |
| `Vault::schema_version` | inside every stored record | the layout a **stored record** was written with |

`create_vault` stamps `SCHEMA_VERSION` into the new record, so every vault
carries a birth certificate for its layout. `schema_version()` exposes the
compiled constant. Comparing the two tells a client or a migrator exactly which
records predate a change — with no out-of-band bookkeeping and no scanning.

---

## Migration strategy

Adding a field to `Vault` changes only the tail of its XDR, so a newer contract
can read an older record and write it back in the new shape. The procedure for a
future layout change:

1. **Append** new fields to `Vault`. Never reorder or retype existing fields —
   that is what would make records genuinely unreadable.
2. **Bump** `types::SCHEMA_VERSION`.
3. **Add a migration entry point**, e.g.

   ```rust
   pub fn migrate_vault(env: Env, vault_id: u64) -> Result<u32, HeirVaultError> {
       let mut vault = storage::load_vault(&env, vault_id)?;
       if vault.schema_version >= SCHEMA_VERSION {
           return Ok(vault.schema_version);       // already current, idempotent
       }
       let from = vault.schema_version;
       // ... fill in the new fields with documented defaults ...
       vault.schema_version = SCHEMA_VERSION;
       storage::save_vault(&env, &vault);
       events::vault_migrated(&env, vault_id, from, SCHEMA_VERSION);
       Ok(SCHEMA_VERSION)
   }
   ```

4. **Make it permissionless and idempotent.** Any caller should be able to
   migrate any vault, and migrating twice must be a no-op. Requiring the owner to
   migrate would mean an abandoned vault — precisely the interesting case — could
   never be brought forward.

Nothing has to be migrated eagerly. Old records stay readable by the new code,
and each one is upgraded the first time somebody touches it. That is why
`schema_version` is stored *per vault* rather than globally: a global flag would
force a stop-the-world migration.

**Version 1 is the only version so far**, so no migration entry point exists yet.
Adding one now would be code with no behaviour behind it, which this project
avoids on principle (see the README's *Planned* section).

---

## Storage-write budget

Counted from `create_vault`, which is the busiest write path:

| Write | Entries |
|---|---|
| allocate vault id | 1 instance |
| save vault record | 1 persistent (+ TTL extend) |
| append owner slot | 1 persistent (+ TTL extend) |
| bump owner count | 1 persistent (+ TTL extend) |
| **total** | **4 ledger entries** |

A mutator (`check_in`, `add_beneficiary`, `claim`, …) is typically:

| Write | Entries |
|---|---|
| touch instance | 0–1 instance (TTL extend only) |
| save vault record | 1 persistent |

Reads are deliberately *not* optimised by caching anything in between calls:
Soroban has no per-transaction memory that survives a call boundary, and any
in-contract memo would become a second source of truth to keep consistent. The
design instead keeps the live state in **one** entry so there is nothing to keep
in sync.

---

## Reading a vault that is not there

`load_vault_opt` returns `Option<Vault>`; `load_vault` maps `None` to
`VaultNotFound`.

Soroban's persistent-storage API does not expose *why* a key is absent, so
`VaultNotFound` conflates three situations that are indistinguishable on-chain:

1. the id was never allocated,
2. the record was archived by the network after its TTL lapsed,
3. (future) the record was explicitly archived.

Consumers must therefore treat `VaultNotFound` as **"no readable vault at this
id"**, not as proof that the vault never existed. Documented on both functions
and on `get_vault`.

The paged `get_vaults_by_owner` is slightly more forgiving: a slot whose vault
cannot be read is **skipped** rather than failing the whole page, so one archived
entry cannot make an owner's other vaults unlistable. `meta.total` still counts
it, so a client sees a gap in `items` rather than a lost page.
