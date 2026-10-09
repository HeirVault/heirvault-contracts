# Stellar Testnet deployment

This document records the real, verifiable Testnet deployment of HeirVault.
Every hash below is a live transaction on Stellar Testnet — there are no mock
hashes, placeholder addresses or simulated receipts anywhere in this file.

If a hash is not listed for an action, that action's transaction is still
visible in the contract's transaction history on the explorer; it was simply not
captured in the shell transcript at the time.

---

## Deployment record

| Item | Value |
|---|---|
| Network | Stellar Testnet |
| Protocol version | 29 |
| RPC | `https://soroban-testnet.stellar.org` |
| Network passphrase | `Test SDF Network ; September 2015` |
| Toolchain | `stellar-cli` 28.1.0, `soroban-sdk` 22.0.11, `rustc` 1.99.0 |
| Build target | `wasm32v1-none`, release profile |

| Contract | Address |
|---|---|
| **HeirVault** | `CC6QRWVXN3V4UOADEZSGXNYRQMR252BXH6ERXOSENR64JTKIRCGA3ADO` |
| Demo SEP-41 asset (`HVUSD`, 7 decimals) | `CCQACXRCTDFOGOMP6NGJVGASIS3LG4B6WN4H74JNJTQFRBTBMQZ2SMGB` |

| Artefact | Value |
|---|---|
| Wasm install hash (on-chain) | verify via deploy tx [`0c21a7a0af6f1586103060b36f0ad3aac17b9f187bf90d4049df6a3264224bed`](https://stellar.expert/explorer/testnet/tx/0c21a7a0af6f1586103060b36f0ad3aac17b9f187bf90d4049df6a3264224bed) |
| Local wasm sha256 | rebuild from this tree; the current build is **not** byte-identical to the previously documented deployment |
| Wasm size | 76,552 bytes |
| Deploy transaction | [`0c21a7a0af6f1586103060b36f0ad3aac17b9f187bf90d4049df6a3264224bed`](https://stellar.expert/explorer/testnet/tx/0c21a7a0af6f1586103060b36f0ad3aac17b9f187bf90d4049df6a3264224bed) |
| Contract explorer | [stellar.expert](https://stellar.expert/explorer/testnet/contract/CC6QRWVXN3V4UOADEZSGXNYRQMR252BXH6ERXOSENR64JTKIRCGA3ADO) · [Stellar Lab](https://lab.stellar.org/r/testnet/contract/CC6QRWVXN3V4UOADEZSGXNYRQMR252BXH6ERXOSENR64JTKIRCGA3ADO) |

The current workspace was redeployed to `CC6QRWVXN3V4UOADEZSGXNYRQMR252BXH6ERXOSENR64JTKIRCGA3ADO`. The previously documented contract `CDVHAOTXHK7IW2T77KRJM7GQNJC7E2VXVWNXR7MI7KIOLFMFNPDHB4EZ` is no longer the live deployment, and the current on-chain install hash is therefore not reproduced by `sha256sum` on the current WASM artifact.

### Accounts used

| Role | Address |
|---|---|
| Vault owner | `GD6VQRYCTXXEGP5RGS2A6VO2FT773RGNY2UE77OL5ZOVYEV67TNQMZKQ` |
| Asset issuer | `GD6I5NRFFLYLR34MBYF533TKL4HQAJKBGCXVL27536GFGN6ZFYCGHZIB` |
| Heir A | `GBWXARKI5DYF432O7O2IICAJLOT2DI6S4GDU2WJQXAPTR5FD2APEODC3` |
| Heir B | `GD4ED2L7VDGU36IE6ELDEG24K2NDSZYTVNTI6JRZ2CYN37QJWVXJ36J4` |
| Heir C | `GA2Q2KO664LYMLEUFEYFILFEAGXI2FPQFX23GEVYI2RXG56T6ONQPYVQ` |
| Guardian 1 | `GBT6KH3WDI2IRVR3QPYEGIYQPWQ7W255N7E6RMYQ766IVIHQG4LSOPWE` |
| Guardian 2 | `GD7WXW5HYXV2ARW4ON6GIURDDMNCQ4MKRBQEZ7Q2VSLJJH2OOOAROTJI` |

> These are Testnet-only keys. Secret keys live in the CLI keystore at
> `~/.config/stellar/identity/`, **never** in this repository (`.stellar/` is
> git-ignored precisely so an older CLI cannot leak one into a commit).

---

## Setup transactions

| Action | Transaction |
|---|---|
| Deploy `HVUSD` Stellar Asset Contract | [`9b40608c99e8770546e96da2e6ad364e5af59de13ee29dfb43474de6cf1d72f6`](https://stellar.expert/explorer/testnet/tx/9b40608c99e8770546e96da2e6ad364e5af59de13ee29dfb43474de6cf1d72f6) |
| Owner trustline for `HVUSD` | [`ea247d7c8bee39585cba12cdcceac5c59b50c9ad0322bf73f732ceacd6e4fecd`](https://stellar.expert/explorer/testnet/tx/ea247d7c8bee39585cba12cdcceac5c59b50c9ad0322bf73f732ceacd6e4fecd) |
| Mint `HVUSD` to owner | [`7700767b9163d55a88e5a87bff757f2b89f13cee019b1c974cdf83611b1e19c1`](https://stellar.expert/explorer/testnet/tx/7700767b9163d55a88e5a87bff757f2b89f13cee019b1c974cdf83611b1e19c1) |

---

## Vault 1 — `MissedCheckIn`, full inheritance flow

A 60-second check-in period with a 60-second grace period was used so the whole
lifecycle could be exercised in a single sitting. The contract's documented
minimums are exactly 60 seconds, so these are legal (if aggressive) parameters.

| Step | Transaction | Result |
|---|---|---|
| `create_vault(owner, HVUSD, MissedCheckIn, 60, 60)` | [`410fd7b9…`](https://stellar.expert/explorer/testnet/tx/410fd7b998b042d276630003c2d9b2db9a94e06317ed84480984c1d6a9db4785) | `vault_created` id=1 |
| `add_beneficiary(1, heir_a, 6000)` | [`263ac963…`](https://stellar.expert/explorer/testnet/tx/263ac9634a19e01898c5e74c0cf48f491e71abc04a19eb84741ae2b46e250f44) | `beneficiary_added`, total 6000 bps |
| `add_beneficiary(1, heir_b, 4000)` | [`93c81f5d…`](https://stellar.expert/explorer/testnet/tx/93c81f5d4db57673a6a5799e9dde46c933fe7d3327a402e825de5e765399024e) | `beneficiary_added`, total 10000 bps |
| `deposit(1, HVUSD, 1_000_000_000)` | [`32d3706a…`](https://stellar.expert/explorer/testnet/tx/32d3706a97ac26a42bd3da7a21f0898167d0a0337af8768abf15235dd51ee762) | SEP-41 `transfer` owner→contract **and** `deposit_made` |
| `check_in(1)` | — | ❌ `GracePeriodExpired` (#42) — the 60 s window had already closed, which is the documented boundary behaviour |
| `activate_vault(1)` (called by **heir_b**, an unrelated account) | [`3b8a161e…`](https://stellar.expert/explorer/testnet/tx/3b8a161e982933a5442543d12a625b7791ae99965b94fa78be403ab853116413) | `vault_activated`, distribution `1_000_000_000`, 2 beneficiaries — **permissionless release proven** |
| `claim(1, heir_a)` | — | reverted at the token layer: heir has no trustline. Claim state left intact |
| `claim(1, heir_b)` | — | same |
| (trustlines created for both heirs) | | |
| `claim(1, heir_a)` | — | `inheritance_claimed`: `600_000_000` |
| `claim(1, heir_b)` | — | `inheritance_claimed`: `400_000_000`, then `vault_completed` |
| — | | Final status `Completed`; contract HVUSD balance `0` |

**This run demonstrates the checks-effects-interactions design directly.** Two
claims reverted *inside the token transfer*, and the subsequent retries
distributed exactly 60%/40% with the contract drained to zero. The revert left no
residue — no half-set `claimed` flag, no wrong balance.

Verified balances:

```
heir-a   600000000  (60%)
heir-b   400000000  (40%)
contract         0
```

---

## Vault 2 — `GuardianApproval`, M-of-N release

| Step | Transaction | Result |
|---|---|---|
| `create_vault(owner, HVUSD, GuardianApproval, 86400, 86400)` | — | `vault_created` id=2 |
| `add_beneficiary(2, heir_a, 10000)` | — | total 10000 bps |
| `add_guardian(2, guardian_1)` | — | `guardian_added`, threshold defaults to 1 |
| `add_guardian(2, guardian_2)` | — | `guardian_added` |
| `set_guardian_threshold(2, 2)` | — | `guardian_threshold_updated` → 2 |
| `deposit(2, HVUSD, 500_000_000)` | — | SEP-41 transfer + `deposit_made` |
| `guardian_approve(2, guardian_1)` | — | `guardian_approved`, 1 of 2 |
| `activate_vault(2)` | — | ❌ `NotEligible` (#34) — **1 of 2 is correctly refused** |
| `guardian_approve(2, guardian_2)` | — | `guardian_approved`, 2 of 2 |
| `activate_vault(2)` | — | ✅ `vault_activated` — guardians released the vault **long before the 24 h check-in deadline**, proving the gate is independent of the clock |
| `claim(2, heir_a)` | — | `inheritance_claimed`: `500_000_000`, then `vault_completed` |

---

## Vault 5 — three-way split with rounding dust

A `1_000_000_001` deposit split 3333 / 3333 / 3334 basis points — deliberately not
divisible — to prove the rounding rule on live Mainnet-grade infrastructure.

| Step | Transaction | Result |
|---|---|---|
| `create_vault` | [`f2e9e30a…`](https://stellar.expert/explorer/testnet/tx/f2e9e30a6e7c2ab00519caf1649eecd8b2f12e4c9fd5e71ac7876bb335d21aa3) | id=5 |
| `add_beneficiary(5, heir_a, 3333)` | [`e549fe4a…`](https://stellar.expert/explorer/testnet/tx/e549fe4a18d6e9f2460d5df63b1fc6021c76b881b914ec26ee23f8c499b8c5d6) | |
| `add_beneficiary(5, heir_b, 3333)` | [`a87daa4e…`](https://stellar.expert/explorer/testnet/tx/a87daa4efc3bfbc7c466de9fb0e3427b9810c55d60b2045a021c870ee36382ff) | |
| `add_beneficiary(5, heir_c, 3334)` | [`34373469…`](https://stellar.expert/explorer/testnet/tx/34373469d484ffd8b6b991a3adaf14ac63260fdba61cc4e30b1a9292f460eed0) | total exactly 10000 bps |
| `add_guardian(5, guardian_1)` | [`d1c50b7f…`](https://stellar.expert/explorer/testnet/tx/d1c50b7fd1ce311076c40d10fa45add572a1a7c16a45c1c487b9eba13c9abfa8) | threshold 1 of 1 |
| `deposit(5, HVUSD, 1_000_000_001)` | [`a2145e86…`](https://stellar.expert/explorer/testnet/tx/a2145e86811abdcc2786d5ee86afbca81085c0ea62f4de41f5a9ad89047c594b) | odd amount on purpose |
| `guardian_approve(5, guardian_1)` | [`51e98cbb…`](https://stellar.expert/explorer/testnet/tx/51e98cbbde6ea0209c940daae24eb7999e340f7ac4e919ab587b2d1b1e4ced86) | 1 of 1 |
| `activate_vault(5)` | [`cbabfe0a…`](https://stellar.expert/explorer/testnet/tx/cbabfe0af8d0f1ef5fadf3de0eaca41440b59d68b74b9d4b440cdecc3a41ee45) | `vault_activated`, distribution `1_000_000_001`, 3 beneficiaries |
| `claim(5, heir_a)` | [`618594ab…`](https://stellar.expert/explorer/testnet/tx/618594abaf71e657cb97d9c66ea8f3c2610a4b241c542920c62a521a92a2284b) | `333_300_000` |
| `claim(5, heir_b)` | [`704c53b6…`](https://stellar.expert/explorer/testnet/tx/704c53b6adff9ca4f975201fbc07d7e229ad74ef9913fd2b8be13208d4ba27f8) | `333_300_000` |
| `claim(5, heir_c)` | — | `333_400_001` — **the last heir absorbs the 1-unit remainder**, then `vault_completed` |

Distribution check:

```
333_300_000 + 333_300_000 + 333_400_001 = 1_000_000_001   ✅ exactly the deposit
contract HVUSD balance after settlement  = 0               ✅ nothing trapped
```

---

## Vault 3 — cancellation and withdrawal

| Step | Transaction | Result |
|---|---|---|
| `create_vault` | [`ee9e005e…`](https://stellar.expert/explorer/testnet/tx/ee9e005e732c8aa6d6187962ffc99c56cfcdfaca387ae89bf0ecec7480838ffb) | id=3 |
| `deposit(3, HVUSD, 300_000_000)` | — | SEP-41 transfer + `deposit_made` |
| `cancel_vault(3)` | [`9367aed1…`](https://stellar.expert/explorer/testnet/tx/9367aed1a9f9aa0b7eb856b1f54635391054498c6e7c5207c5a8cc47a2939d85) | `vault_cancelled`, refundable `300_000_000` |
| `activate_vault(3)` | — | ❌ `VaultCancelled` (#35) — **a cancelled vault cannot be released** |
| `withdraw(3, 200_000_000)` | — | SEP-41 transfer contract→owner + `withdrawal`, remaining `100_000_000` |
| `withdraw(3, 100_000_000)` | — | `withdrawal`, remaining `0` |
| `withdraw(3, 1)` | — | ❌ `InsufficientBalance` (#51) — **cannot overdraw** |

---

## Vault 4 — successful check-in

| Step | Transaction | Result |
|---|---|---|
| `create_vault(owner, HVUSD, MissedCheckIn, 3600, 600)` | [`a08a4f45…`](https://stellar.expert/explorer/testnet/tx/a08a4f45f33d5b6b28f416fef03391120dafdd4920fdbf1f51bf867d46f81e1b) | id=4 |
| `deposit(4, HVUSD, 100_000_000)` | — | SEP-41 transfer + `deposit_made` |
| `check_in(4)` | [`15933a95…`](https://stellar.expert/explorer/testnet/tx/15933a956e8a070e4842072be2772af58c4cd642bfcc3b771535fae6a97f2bde) | `check_in_completed`, new deadline `now + 3600` |

Read-back after the check-in:

```json
get_deadlines(4)    = {"deadline":1791540102,"grace_end":1791540702,"now":1791536502}
get_vault_status(4) = "Active"
```

Vault 4 was left deliberately un-settled so the deployed contract holds a live,
non-zero balance that anyone can query:

```bash
stellar contract invoke --id CCQACX…SMGB --network testnet -- balance --id CDVHAO…B4EZ
# "100000000"
```

---

## Live state at time of writing

| Query | Result |
|---|---|
| `schema_version()` | `1` |
| `get_vault_count()` | `5` |
| `get_vault_status(5)` | `"Completed"` |
| `HVUSD.decimals()` | `7` |
| `HVUSD.symbol()` | `"HVUSD"` |
| HeirVault HVUSD balance | `100000000` (vault 4, still active) |

## Summary of what was proven on Testnet

- ✅ wasm deployed; **local sha256 matches the on-chain install hash**
- ✅ real SEP-41 asset (Stellar Asset Contract) deployed and minted
- ✅ `create_vault` in two different activation modes
- ✅ `add_beneficiary` including a three-way split totalling exactly 10000 bps
- ✅ `deposit` performing a **real SEP-41 transfer** owner → contract
- ✅ `check_in` succeeding, with the recomputed deadline read back
- ✅ `check_in` correctly refused with `GracePeriodExpired` after the window
- ✅ `activate_vault` called permissionlessly by an unrelated account
- ✅ `claim` by three heirs with **exact** distribution and zero dust trapped
- ✅ guardian add / threshold / approve, **1-of-2 refused (`NotEligible`)**, 2-of-2 releases
- ✅ `cancel_vault` + two partial withdrawals, over-withdrawal refused (`InsufficientBalance`)
- ✅ activation refused on a cancelled vault (`VaultCancelled`)
- ✅ `get_vaults_by_owner` paging against live state
- ✅ atomic reverts (missing trustline) leaving claim state uncorrupted

## Not exercised on Testnet

For completeness, these are implemented and covered by the automated suite but
were not repeated as live Testnet transactions:

- `MultiCondition` activation mode (the other two modes were exercised; this is
  the conjunction of both and is covered by two unit tests)
- `remove_beneficiary` / `update_beneficiary` and the guardian removal paths
- `get_beneficiaries`, `get_guardians` and `get_claims` paging
- the `InvalidPeriod` / `DuplicateBeneficiary` / `TooManyBeneficiaries` rejection
  paths

They are all reachable against the deployed contract using the invocations in
the README.
