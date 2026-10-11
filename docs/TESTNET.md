# Stellar Testnet deployment

This document records the real, verifiable Testnet deployment of HeirVault.
Every hash below is a live transaction on Stellar Testnet — there are no mock
hashes, placeholder addresses or simulated receipts anywhere in this file.

Unlike the earlier revision recorded in this repository, the deployed wasm now
**matches the source in this tree**: the on-chain `ContractExecutable::Wasm` hash
equals `sha256` of `target/wasm32v1-none/release/heirvault.wasm` built from this
commit. See [Artefact verification](#artefact-verification).

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
| **HeirVault** | `CB4I2OSSNXQCH3DGHKGYMG5DPDOUDD55YG3SSDOK4JICVDH5B7KLEWI6` |
| Demo SEP-41 asset (`HVUSD`, 7 decimals) | `CCOYBUYUFH4LABTD5ZQB6FFCC4VEUWQOPSX6G2AVHWNX3YAY476AGPBX` |

### Artefact verification

| Artefact | Value |
|---|---|
| Wasm hash **on-chain** (read via `getLedgerEntries` on the contract-instance key) | `b1b2a61c01e6d77ac0daf1c7150d8e87a6af92375ffdc803d54755ed71dc9816` |
| Wasm hash **from this source** | `b1b2a61c01e6d77ac0daf1c7150d8e87a6af92375ffdc803d54755ed71dc9816` — **match** |
| Wasm size | 79,154 bytes |
| Deploy transaction | [`3333aa5a7a92f600b009f8c7a8c12f0c546dc59c995ed617dabfb30a0ca8d068`](https://stellar.expert/explorer/testnet/tx/3333aa5a7a92f600b009f8c7a8c12f0c546dc59c995ed617dabfb30a0ca8d068) |
| Contract explorer | [stellar.expert](https://stellar.expert/explorer/testnet/contract/CB4I2OSSNXQCH3DGHKGYMG5DPDOUDD55YG3SSDOK4JICVDH5B7KLEWI6) · [Stellar Lab](https://lab.stellar.org/r/testnet/contract/CB4I2OSSNXQCH3DGHKGYMG5DPDOUDD55YG3SSDOK4JICVDH5B7KLEWI6) |

The hash was checked by fetching the contract-instance ledger entry over RPC and
decoding the `ContractExecutable` with `stellar-xdr`:

```bash
# instance key = LedgerKey::ContractData(ScAddress::Contract(CB4I2…), LedgerKeyContractInstance, persistent)
curl -s -X POST https://soroban-testnet.stellar.org -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getLedgerEntries","params":{"keys":["<base64 key>"]}}'
# → ContractExecutable::Wasm(b1b2a61c01e6d77ac0daf1c7150d8e87a6af92375ffdc803d54755ed71dc9816)
```

### Accounts used

| Role | Address |
|---|---|
| Vault owner (deployer) | `GCFLA6JA5KI4753WPOJL3APSFXDWAZS5OLMEULKUMX73SZNT2523TQFP` |
| Asset issuer | `GCK6EV62EOACYQ5YDGT6UPAZ563XIHYV2IAQALBHRDLWNC44GYCOL3PK` |
| Heir A | `GBXRSGJHD3NTYZEHOGESC3W7AWIE6VSQHBR5UCV4W2SPUWC3OOT5IW3I` |
| Heir B | `GAFDMG74PSA56TVICYUAT57PM34BURAAQNHUJSEV5HRRJT2XUQTIQ3AD` |
| Heir C | `GBD774HY4T7DT5H3TXWIXKMJLDVC64XULCIGSTFUNCMDTFCCRLNT44BX` |
| Guardian 1 | `GC32V4HCLG2EWX5N7IKWGJMMLW5BM5EGJ6X33OXAAFWEO6L3CUWMS7M6` |
| Guardian 2 | `GAOUGZAQQKD3ZXWCWAVW6UTHUHSWJFIIHAB7PSVWPQI6FIJ6WAQVKZNL` |

> These are Testnet-only keys created for this run. Secret keys live in the CLI
> keystore at `~/.config/stellar/identity/`, **never** in this repository
> (`.stellar/` and `*.secret` are git-ignored precisely so an older CLI cannot
> leak one into a commit).

---

## Setup transactions

| Action | Transaction |
|---|---|
| Deploy `HVUSD` Stellar Asset Contract | [`5bdf4713c38cc44a90a98c58c577adecca87eeb6bf557c0127b976ced8d7beef`](https://stellar.expert/explorer/testnet/tx/5bdf4713c38cc44a90a98c58c577adecca87eeb6bf557c0127b976ced8d7beef) |
| Owner trustline for `HVUSD` | [`410c20d41b1f2cb1b73b02eb2e0c2350bf099e3cd528032d5b903d527b8f4d54`](https://stellar.expert/explorer/testnet/tx/410c20d41b1f2cb1b73b02eb2e0c2350bf099e3cd528032d5b903d527b8f4d54) |
| Mint `100,000,000,000` `HVUSD` to owner | [`80fbf60de2a20a88fe4f9c2575334e666d8dd81c5b9b82f4f7554603dba2317e`](https://stellar.expert/explorer/testnet/tx/80fbf60de2a20a88fe4f9c2575334e666d8dd81c5b9b82f4f7554603dba2317e) |

Trustlines were also created for heirs A, B and C so their claims can settle.

---

## Walkthrough

Nine operations are refused-path checks (they never land on-chain and therefore
have no hash); everything else has a real transaction below. Vault ids are the
on-chain ids returned by `create_vault`.

### Vault 1 — `MissedCheckIn`, full inheritance flow

A 60-second check-in period with a 60-second grace period was used so the whole
lifecycle could be exercised in a single sitting; both are exactly the contract's
documented minimums.

| Step | Transaction | Result |
|---|---|---|
| `create_vault(owner, HVUSD, MissedCheckIn, 60, 60)` | [`d7a45d4f…`](https://stellar.expert/explorer/testnet/tx/d7a45d4f363ef83a2ec5aab515c1c58812b7b6d10e55415099fa12f973a29298) | `vault_created` id=1 |
| `add_beneficiary(1, heir_b, 4000)` | [`80b1f20a…`](https://stellar.expert/explorer/testnet/tx/80b1f20adefcde0d19849e9ea80016ee9463ece1ae46c96572f22324bbf826a1) | `beneficiary_added` |
| `add_beneficiary(1, heir_a, 6000)` | [`6c8580c0…`](https://stellar.expert/explorer/testnet/tx/6c8580c079d0a7561df3bf0b191af39c18db54f9cd4a3dd966944856c896892c) | total 10000 bps |
| `deposit(1, HVUSD, 1_000_000_000)` | [`a7f173f7…`](https://stellar.expert/explorer/testnet/tx/a7f173f701319151796988b307505d4879247ce0e5608e3668f3715e102da03b) | SEP-41 transfer owner→contract + `deposit_made` |
| `check_in(1)` | — | ❌ `GracePeriodExpired` (#42) — the 60 s window had closed |
| `activate_vault(1)` called by **guardian 2** (no role in this vault) | [`096329c4…`](https://stellar.expert/explorer/testnet/tx/096329c49ae2516f02578afc8f08c9a7b2f80716ea4e036474a01c747c5e4f33) | `vault_activated`, distribution `1_000_000_000` — **permissionless release proven** |
| `claim(1, heir_a)` | [`8f210381…`](https://stellar.expert/explorer/testnet/tx/8f21038180a16b958fc3661f723b4df754c0da2071df26395f52b0e3bef1ddfa) | `inheritance_claimed`: `600_000_000` |
| `claim(1, heir_b)` | [`830868be…`](https://stellar.expert/explorer/testnet/tx/830868be129d6ebe69eb27207856fa30ffd06d430a122ce7e7c1609cd596e37b) | `inheritance_claimed`: `400_000_000`, then `vault_completed` |

Final status `Completed`; vault 1's share of the contract balance reaches `0`.

### Vault 2 — `MissedCheckIn`, unfunded edge case

| Step | Result |
|---|---|
| `create_vault(owner, HVUSD, MissedCheckIn, 60, 60)` — [`69398ffb…`](https://stellar.expert/explorer/testnet/tx/69398ffb72d65d19fdac5c496108edb0df15f76396c5c34df0d064b7c47a4712) | id=2 |
| after the grace window, `activate_vault(2)` | ❌ `NoBeneficiaries` (#15) — an empty, unfunded vault can never be released |

### Vault 3 — `GuardianApproval`, M-of-N release

| Step | Transaction | Result |
|---|---|---|
| `create_vault(..., GuardianApproval, 86400, 86400)` | [`62a4c327…`](https://stellar.expert/explorer/testnet/tx/62a4c32778b0781c446a71be281c683575cdf036a7ff7ae9427778407a340b64) | id=3 |
| `add_beneficiary(3, heir_a, 10000)` | [`1e4b6974…`](https://stellar.expert/explorer/testnet/tx/1e4b69746ab06dec9633d249a76e5735d73a11aaf27e43fdb82ada37d22d7baa) | total 10000 bps |
| `add_guardian(3, guardian_1)` | [`92269b71…`](https://stellar.expert/explorer/testnet/tx/92269b71d1295e131128bc8e523aac843a8adee31973cffae71a5bc819eb51c8) | threshold defaults to 1 |
| `add_guardian(3, guardian_2)` | [`d08628f9…`](https://stellar.expert/explorer/testnet/tx/d08628f955312ce180b24342892a1574c37be785ac1769ee6234cd89aa7edcca) | |
| `set_guardian_threshold(3, 2)` | [`c9bd4f57…`](https://stellar.expert/explorer/testnet/tx/c9bd4f57ccf462d7e74e4270752d30172ed73be61de97fe201f03bd4c61b3ed8) | `guardian_threshold_updated` → 2 |
| `deposit(3, HVUSD, 500_000_000)` | [`61251958…`](https://stellar.expert/explorer/testnet/tx/6125195800986fb44e54e3085d1527f312a45bbb2647efca9f351c23740cecd6) | |
| `guardian_approve(3, guardian_1)` | [`b2a0423f…`](https://stellar.expert/explorer/testnet/tx/b2a0423f559dd1251477fefbcf1f0efc76671f600470f59264ad3caff01d0a54) | 1 of 2 |
| `activate_vault(3)` | — | ❌ `NotEligible` (#34) — **1 of 2 is correctly refused** |
| `guardian_approve(3, guardian_2)` | [`0c66649f…`](https://stellar.expert/explorer/testnet/tx/0c66649f4205880b25c6fd5c6122168ddd74c51f05851db3f8f15d1dcf8a210f) | 2 of 2 |
| `activate_vault(3)` | [`aa241a63…`](https://stellar.expert/explorer/testnet/tx/aa241a63038bffe0482ec882f3dad4a55988ee5b818aa8b21643f42b5630a471) | ✅ released **inside** the 24 h check-in window — the gate is independent of the clock |
| `claim(3, heir_a)` | [`efd5796b…`](https://stellar.expert/explorer/testnet/tx/efd5796bc0e8975534f48f479b256d8a842f0b96035b319e63b284d28a4a6259) | `500_000_000`, then `vault_completed` |

### Vault 4 — cancellation and withdrawal

| Step | Transaction | Result |
|---|---|---|
| `create_vault(..., MissedCheckIn, 3600, 600)` | [`eed3ea31…`](https://stellar.expert/explorer/testnet/tx/eed3ea31ee965760e1cb4521dea68417322f0f2a79033f32593acc601772461a) | id=4 |
| `deposit(4, HVUSD, 300_000_000)` | [`44a4ef44…`](https://stellar.expert/explorer/testnet/tx/44a4ef4439e9b17d63958bafb95ce14f309fd8b39a56ca633e2b9f6aa10cd73b) | |
| `cancel_vault(4)` | [`9544ebbc…`](https://stellar.expert/explorer/testnet/tx/9544ebbc6ef70b68c2771cf71be9035de28a0803326567eda346c4f456102b56) | `vault_cancelled`, refundable `300_000_000` |
| `withdraw(4, 200_000_000)` | [`66773c82…`](https://stellar.expert/explorer/testnet/tx/66773c82e78a82f06d025aa5734fcbc2585ad0e2cf74d8161434a3fe4e7916a8) | SEP-41 transfer contract→owner |
| `withdraw(4, 100_000_000)` | [`111bb274…`](https://stellar.expert/explorer/testnet/tx/111bb2744ca4ecc95b0f1a34114a6f5b51e1f790d2a69ff031d89ccaa0f56f63) | remaining `0` |
| `withdraw(4, 1)` | — | ❌ `InsufficientBalance` (#51) — **cannot overdraw** |
| `activate_vault(4)` | — | ❌ `VaultCancelled` (#35) — **a cancelled vault cannot be released** |

### Vault 5 — successful check-in

| Step | Transaction | Result |
|---|---|---|
| `create_vault(..., MissedCheckIn, 3600, 600)` | [`45a406a5…`](https://stellar.expert/explorer/testnet/tx/45a406a521da4e0af6a39fb2b723ea645efaaed2ad0d4b5c122d7f24b63563d4) | id=5 |
| `deposit(5, HVUSD, 100_000_000)` | [`42d8ff98…`](https://stellar.expert/explorer/testnet/tx/42d8ff98457b0170e77291dd318fd54e3fc9b576fa1206b84ef31aec75e35cfc) | |
| `check_in(5)` | [`1ab67fc8…`](https://stellar.expert/explorer/testnet/tx/1ab67fc8c73be7ef86347d12625074de019c4ba0876c411eba9c8902a78f8747) | `check_in_completed` |

Read-back after the check-in:

```json
get_deadlines(5) = {"deadline":1791683907,"grace_end":1791684507,"now":1791680307}
```

Vault 5 was deliberately left unsettled so the deployed contract holds a live,
non-zero balance that anyone can query:

```bash
stellar contract invoke --contract-id CCOYBUYUFH4LABTD5ZQB6FFCC4VEUWQOPSX6G2AVHWNX3YAY476AGPBX \
  --source heirvault-deployer --network testnet -- balance \
  --id CB4I2OSSNXQCH3DGHKGYMG5DPDOUDD55YG3SSDOK4JICVDH5B7KLEWI6
# "100000000"
```

### Vault 6 — three-way split with rounding dust

A `1_000_000_001` deposit split 3333 / 3333 / 3334 basis points — deliberately not
divisible — released via a single 1-of-1 guardian.

| Step | Transaction | Result |
|---|---|---|
| `create_vault(..., GuardianApproval, 86400, 86400)` | [`86f2e918…`](https://stellar.expert/explorer/testnet/tx/86f2e9187600c90404c1257b41f6affcffc577d4aa7a4ffac6537096dfe6f6b0) | id=6 |
| `add_beneficiary(6, heir_a, 3333)` | [`f041830c…`](https://stellar.expert/explorer/testnet/tx/f041830cdd1cdbf6e3d0580a68a4c82ff006711d2f864a1067e644efc0643358) | |
| `add_beneficiary(6, heir_b, 3333)` | [`6803722b…`](https://stellar.expert/explorer/testnet/tx/6803722b5343221f05f5fbeb924e0e982cc9bc03241a08e026d951a36d8a2b9c) | |
| `add_beneficiary(6, heir_c, 3334)` | [`44e83999…`](https://stellar.expert/explorer/testnet/tx/44e8399935dfbca5e3181c1d24c5eb1b2ce07ab008cffde6c01a71090b27eb10) | total exactly 10000 bps |
| `add_guardian(6, guardian_1)` | [`4772a762…`](https://stellar.expert/explorer/testnet/tx/4772a762210a47fdd3f900e8605169624ded26eac2b73cc07daaac70bd1884c8) | threshold 1 of 1 |
| `deposit(6, HVUSD, 1_000_000_001)` | [`c406f28f…`](https://stellar.expert/explorer/testnet/tx/c406f28f1dc4bfd5a8673bfed87b82fa7510d5524d1ca34a56dff667bf9e0c81) | odd amount on purpose |
| `guardian_approve(6, guardian_1)` | [`cb3eba11…`](https://stellar.expert/explorer/testnet/tx/cb3eba1187c1b47feb734da943344b0b4fa5a0352b892b671516238ead867886) | 1 of 1 |
| `activate_vault(6)` | [`9e12c8ac…`](https://stellar.expert/explorer/testnet/tx/9e12c8acfc323ffeec37e73551097e566f18b356b9a00827a0b20fa2b44b4a4a) | distribution `1_000_000_001`, 3 beneficiaries |
| `claim(6, heir_a)` | [`734ea959…`](https://stellar.expert/explorer/testnet/tx/734ea959cfe604c4610137500bcdf7e89740a505b6cc0469f95cab601ba8224f) | `333_300_000` |
| `claim(6, heir_b)` | [`bf7bc4ad…`](https://stellar.expert/explorer/testnet/tx/bf7bc4adbb9a23d91aa441d929cd8f5868a11bc83f4dcb61386262f8b4f93f6e) | `333_300_000` |
| `claim(6, heir_c)` | [`2133b806…`](https://stellar.expert/explorer/testnet/tx/2133b8063841415090624a7fad35dec96e83d06e56df4a030c4399d501f47a22) | `333_400_001` — **the last heir absorbs the 1-unit remainder**, then `vault_completed` |

Distribution check:

```text
333_300_000 + 333_300_000 + 333_400_001 = 1_000_000_001   ✅ exactly the deposit
```

---

## Live state at time of writing

Verified asset balances (RPC reads of the `HVUSD` SAC):

| Holder | HVUSD | Composed of |
|---|---|---|
| Owner | `97_399_999_999` | minted 100 bn, minus net deposits (2.9 bn) plus the 0.3 bn withdrawn from vault 4 |
| Heir A | `1_433_300_000` | vault 1 `600_000_000` + vault 3 `500_000_000` + vault 6 `333_300_000` |
| Heir B | `733_300_000` | vault 1 `400_000_000` + vault 6 `333_300_000` |
| Heir C | `333_400_001` | vault 6 `333_400_001` |
| **HeirVault contract** | `100_000_000` | vault 5, still `Active` |

| Query | Result |
|---|---|
| `schema_version()` | `1` |
| `get_vault_count()` | `6` |
| `get_vault_status(1)` | `"Completed"` |
| `get_vault(5)` | `balance 100000000`, `Active` |
| `HVUSD.decimals()` | `7` |
| `get_vaults_by_owner(owner, 0, 3)` | ids `[1, 2, 3]`, `has_more: true`, `next_offset: 3` |
| `get_vaults_by_owner(owner, 3, 3)` | ids `[4, 5, 6]`, `has_more: false` |
| `get_vaults_by_beneficiary(heir_a, 0, 10)` | ids `[3, 6, 1]`, total `3` |

## Summary of what was proven on Testnet

- ✅ wasm redeployed; the **on-chain install hash equals a local build of this source** (`b1b2a61c…`)
- ✅ real SEP-41 asset (Stellar Asset Contract) deployed and minted
- ✅ `create_vault` in two different activation modes, plus an unfunded vault that could not be released
- ✅ `add_beneficiary` including a three-way split totalling exactly 10000 bps
- ✅ `deposit` performing a **real SEP-41 transfer** owner → contract
- ✅ `check_in` succeeding, with the recomputed deadline read back
- ✅ `check_in` correctly refused with `GracePeriodExpired` (#42) after the window
- ✅ `activate_vault` called permissionlessly by an account with no role in the vault
- ✅ `guardian_approve`, with 1-of-2 refused (`NotEligible` #34) and 2-of-2 releasing
- ✅ `claim` by three heirs with **exact** distribution and zero dust trapped
- ✅ `cancel_vault` + two partial withdrawals; over-withdrawal refused (`InsufficientBalance` #51)
- ✅ activation refused on a cancelled vault (`VaultCancelled` #35)
- ✅ `get_vaults_by_owner` and `get_vaults_by_beneficiary` paging against live state

## Not exercised on Testnet

For completeness, these are implemented and covered by the automated suite but
were not repeated as live Testnet transactions:

- `MultiCondition` activation mode (the conjunction of the other two; covered by
  unit tests)
- `remove_beneficiary` / `update_beneficiary` and the guardian-removal paths
- `get_claims`, `get_guardians` and the remaining read entry points
- re-entrancy / hostile-token behaviour (covered in-crate by `ReentrantToken` and
  `FailingToken`, not by a live transaction — a hostile asset contract cannot be
  deployed to Testnet to exercise it)

---

## Reproducing this walkthrough

`stellar-cli` 28 renamed `contract invoke --id` to `--contract-id` and requires
`--source-account` even for reads. The sequence below matches this record.

```bash
export CID=CB4I2OSSNXQCH3DGHKGYMG5DPDOUDD55YG3SSDOK4JICVDH5B7KLEWI6
export SAC=CCOYBUYUFH4LABTD5ZQB6FFCC4VEUWQOPSX6G2AVHWNX3YAY476AGPBX
export OWNER=$(stellar keys address heirvault-deployer)

# 0. Build and deploy (must reproduce the on-chain hash above).
cargo build -p heirvault --release --target wasm32v1-none
stellar contract deploy --wasm target/wasm32v1-none/release/heirvault.wasm \
  --source heirvault-deployer --network testnet

# 1. Create a vault: 30-day check-in, 7-day grace.
stellar contract invoke --contract-id $CID --source heirvault-deployer --network testnet -- \
  create_vault --owner $OWNER --asset $SAC \
  --activation_mode MissedCheckIn --check_in_period 2592000 --grace_period 604800

# 2. Name heirs (allocations must total 10000 bps before activation).
stellar contract invoke --contract-id $CID --source heirvault-deployer --network testnet -- \
  add_beneficiary --vault_id 1 --beneficiary <HEIR_A> --allocation_bps 6000
stellar contract invoke --contract-id $CID --source heirvault-deployer --network testnet -- \
  add_beneficiary --vault_id 1 --beneficiary <HEIR_B> --allocation_bps 4000

# 3. Fund it.
stellar contract invoke --contract-id $CID --source heirvault-deployer --network testnet -- \
  deposit --vault_id 1 --asset $SAC --amount 1000000000

# 4. Keep it alive.
stellar contract invoke --contract-id $CID --source heirvault-deployer --network testnet -- \
  check_in --vault_id 1

# 5. Miss the window, then anyone may release.
stellar contract invoke --contract-id $CID --source <any-key> --network testnet -- \
  activate_vault --vault_id 1

# 6. Each heir claims their own share.
stellar contract invoke --contract-id $CID --source <heir-a-key> --network testnet -- \
  claim --vault_id 1 --beneficiary <HEIR_A>
```
