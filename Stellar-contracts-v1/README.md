# Stellar-contracts-v1

Soroban contracts deployed on **Stellar** (testnet/mainnet) for the PUSD decentralized reserve bridge.

The relayer that mints wPi after Pi deposits are observed on Pi Network, and
that releases Pi on wPi redemption, lives in [`../relayer`](../relayer/README.md).

| Crate        | WASM artifact   | Purpose                                      |
|-------------|-----------------|----------------------------------------------|
| `wpi-token` | `wpi_token.wasm` | Wrapped Pi minted by the relayer after Pi deposits |
| `mock-amm` | `mock_amm.wasm` | Test AMM that swaps wPi against the network's real USDC SAC |

## Requirements

- Rust stable + `wasm32-unknown-unknown` target
- Soroban CLI aligned with **soroban-sdk 23.0.1** (same as `Pusd-contracts-v1`)

## Build

From the repository root:

```bash
make build
```

Artifacts: `Stellar-contracts-v1/target/wasm32-unknown-unknown/release/*.wasm`

## Deploy and Release

Use the checked-in deployment scripts from the repository root. They build the
WASM artifacts, upload each contract with the Stellar CLI, deploy from the
uploaded WASM hash, and initialize the contracts.

```bash
make deploy-testnet
```

For mainnet, provide the signing identity and RPC endpoint explicitly:

```bash
STELLAR_ACCOUNT=<identity> STELLAR_RPC_URL=<mainnet-rpc-url> make deploy-mainnet
```

The testnet script deploys and initializes `wpi-token` and `mock-amm`; the AMM
selects the network's real USDC SAC from the ledger network ID. The mainnet
script deploys only `wpi-token` by default; set `DEPLOY_AMM=true` to deploy the
AMM against mainnet USDC as well.

### Admin roles: minter, pauser, upgrader/admin, volume-limit admin

`wpi-token` splits privileged access into four independent roles instead of a
single admin key. Each role is a Soroban `Address`, so it can be a plain
account **or a contract address** — a Soroban multisig/policy contract, or an
account using Protocol 27 (CAP-0071) native authentication delegation
(`SOROBAN_CREDENTIALS_ADDRESS_V2`). The contract never special-cases which
kind of address a role holds; `require_auth()` is uniform for both.

| Role | Storage key | Gates | Rotated by |
|---|---|---|---|
| Admin | `Admin` | `propose_admin`/`accept_admin` (two-step), `upgrade` | Two-step: current admin proposes, proposed admin accepts |
| Minter | `Minter` | `mint`, `mint_from_deposit`, `burn` | Current minter only (`set_minter`) |
| Pauser | `Pauser` | `set_paused` | Current pauser only (`set_pauser`) |
| Volume-limit admin | `VolumeLimitAdmin` | `configure_volume_limits`, `override_volume_limit`, `set_volume_limit_admin` | Current volume-limit admin only |

All four default to the deployer's admin identity at `initialize`. **Rotate
each of them independently to dedicated multisig/governance addresses before
routing real bridge traffic** — see
[`docs/design/admin-role-separation.md`](../docs/design/admin-role-separation.md)
for the full policy and the migration plan for existing deployments.
Deliberately, the top-level admin **cannot** reclaim the minter or pauser role
by calling `set_minter`/`set_pauser` itself once it has been handed off; only
the current holder of each role can rotate it. This means a compromised admin
key alone cannot re-seize mint power, and a compromised minter can still be
halted by the independent pauser:

```bash
stellar contract invoke --id "$WPI_CONTRACT_ID" --source "$ADMIN_IDENTITY" \
  --network testnet -- set_minter --new_minter "$BRIDGE_OPS_MULTISIG"

stellar contract invoke --id "$WPI_CONTRACT_ID" --source "$ADMIN_IDENTITY" \
  --network testnet -- set_pauser --new_pauser "$GUARDIAN_MULTISIG"
```

### Emergency pause behavior

The pause flag is a full emergency stop for token state changes. While paused,
`approve`, `transfer`, `transfer_from`, `mint`, and `burn` return `Paused` in
both `wpi-token` and contracts built on `soroban-token-common`. Read-only calls
remain available. Only the authorized pauser can change the pause state; for a
volume-limit halt, governance must use the auditable override flow described
below before activity can resume.

### Storage TTL / rent model

`wpi-token` deliberately splits storage by lifecycle:

| Data | Storage class | TTL strategy |
|---|---|---|
| `Balance(owner)`, `Allowance(owner, spender)`, processed deposit IDs, processed redemption IDs | Persistent | Each write extends that entry's TTL independently |
| `Admin`, `ProposedAdmin`, `Minter`, `Pauser`, `VolumeLimitAdmin`, `Paused`, `CircuitBreaker`, `TotalSupply`, volume-window bookkeeping | Instance | The contract refreshes instance TTL on writes; operators must still bump it during long idle periods |

This matters because Soroban `instance()` storage has a single shared TTL for
the whole contract instance. If it is allowed to age out, every value stored in
that namespace becomes unavailable together. User-owned balances and allowances
therefore live in `persistent()` storage instead, so one stale account can no
longer drag every holder over the same expiry cliff.

For quiet periods, run an automated keeper or cron job that calls the admin-only
`bump_instance_ttl` entrypoint before the shared instance TTL gets close to
expiry. Example:

```bash
stellar contract invoke \
  --id "$WPI_CONTRACT_ID" \
  --source "$ADMIN_IDENTITY" \
  --network testnet \
  -- \
  bump_instance_ttl
```

Recommended operations policy:

- Trigger `bump_instance_ttl` on a schedule comfortably inside the rent window
  even if no bridge traffic is flowing.
- Monitor the contract instance TTL off-chain and alert well before expiry.
- Keep rent funding and the admin signer used for this keeper path under the
  same operational controls as upgrade governance.

### Configure the wPi bridge volume circuit breaker

`wpi-token` fails closed: mint and burn calls return
`VolumeLimitsNotConfigured` until the admin configures positive limits. Amounts
are expressed in wPi stroops (7 decimals), and the window is expressed in
seconds. Immediately after initialization, transfer the independent limit-admin
role from the deployer to the bridge multisig or governance contract:

```bash
stellar contract invoke \
  --id "$WPI_CONTRACT_ID" \
  --source "$ADMIN_IDENTITY" \
  --network testnet \
  -- \
  set_volume_limit_admin \
  --new_admin "$MULTISIG_ADDRESS"
```

The multisig then configures a 24-hour window:

```bash
stellar contract invoke \
  --id "$WPI_CONTRACT_ID" \
  --source "$MULTISIG_IDENTITY" \
  --network testnet \
  -- \
  configure_volume_limits \
  --mint_limit 10000000000000 \
  --burn_limit 10000000000000 \
  --window_seconds 86400
```

The contract maintains separate mint and burn totals using a bounded rolling
window with up to 24 subdivisions plus one conservative boundary bucket. An
operation that reaches the threshold is accepted and pauses the contract; an
operation that would exceed it returns `false` without changing balances or
marking its deposit processed. Both paths emit `VolumeLimitTriggered`, whose
`accepted` field distinguishes them, and later operations return `Paused`.
The over-limit call returns successfully at the transaction layer so the pause
and alert event are committed atomically instead of being rolled back; the
relayer reads the boolean result and keeps a rejected deposit pending for retry.

Only the address stored as `volume_limit_admin` can lift this halt. This role is
separate from the bridge admin used for mint/burn, so a compromised relayer
cannot reconfigure or clear the limit after governance has taken ownership.
After review and multisig approval, it calls:

```bash
stellar contract invoke \
  --id "$WPI_CONTRACT_ID" \
  --source "$MULTISIG_IDENTITY" \
  --network testnet \
  -- \
  override_volume_limit
```

`override_volume_limit` clears both rolling totals, starts a fresh window,
unpauses the contract, and emits `VolumeLimitOverride`. Calling
`set_paused(..., false)` while the circuit breaker is active is rejected, so
the auditable override path cannot be bypassed.

Set backend env:

- `STELLAR_SOROBAN_RPC_URL` — e.g. `https://soroban-testnet.stellar.org`
- `STELLAR_NETWORK_PASSPHRASE` — Stellar testnet passphrase
- `WPI_CONTRACT_ID` — deployed wPi contract ID
- `USDC_CONTRACT_ID` — selected network USDC SAC (do not deploy it)
- `BRIDGE_STELLAR_ADMIN_SECRET_KEY` — the relayer's mint key; it must hold the
  `Minter` role (not necessarily the top-level `Admin` role — see
  [role table above](#admin-roles-minter-pauser-upgraderadmin-volume-limit-admin))
  and should be rotated to a dedicated multisig or delegated account in production

## Real USDC SAC

The AMM uses `soroban_sdk::token::Client` with the canonical Stellar Asset
Contract for Circle-issued USDC. It resolves the address at runtime from
`env.ledger().network_id()`:

| Network | Passphrase | Circle USDC issuer | USDC SAC |
|---|---|---|---|
| Testnet | `Test SDF Network ; September 2015` | `GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5` | `CAQCMV4JFG4EZXQEAV7TUV2E52DMSO2LQKBOSA7UM3B4NIP4DQJ3JHQJ` |
| Mainnet | `Public Global Stellar Network ; September 2015` | `GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN` | `CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75` |

Mainnet values were checked against Stellar Expert's Circle USDC record.
Unsupported networks fail initialization instead of selecting the wrong asset.
Offline tests register a local SAC with
`env.register_stellar_asset_contract_v2(...)`.



## Quickstart: full testnet flow

Run the scripted walkthrough to build and exercise the complete testnet path. It uses real Stellar/Soroban CLI commands against testnet, creates/funds fresh identities when needed, and prints the expected successful output after each step:

```bash
cd Stellar-contracts-v1
./scripts/quickstart.sh
```

The script deploys `wpi-token` and `mock-amm`, then runs initialize → mint wPi
→ transfer → real-USDC liquidity deposit → swap. The admin identity must
already hold enough testnet USDC, available from Circle's faucet. Override
identities, amounts, or network settings with environment variables such as
`ADMIN_IDENTITY`, `RECIPIENT_IDENTITY`, `RPC_URL`, `MINT_AMOUNT`, and
`SWAP_AMOUNT`.

These same values, plus the Pi Network side, configure the relayer — see
[`../relayer/.env.example`](../relayer/.env.example).


## WASM Size Tracking

CI reports the compiled WASM size for each contract on every PR and compares it against the committed baseline in [`wasm-size-baseline.json`](./wasm-size-baseline.json).

| Contract | Baseline file key |
|---|---|
| `wpi-token` | `wpi_token` |
| `mock-amm` | `mock_amm` |

A contract that grows by more than **5 %** relative to its baseline will fail the `Check WASM size regressions` step. The full size table and diff are posted to the **Job Summary** tab in GitHub Actions.

### Setting / updating the baseline

After an intentional size change (new feature, dependency bump, etc.), refresh the baseline:

```bash
# 1. Build release WASMs
cd Stellar-contracts-v1
cargo build --target wasm32-unknown-unknown --release

# 2. Regenerate the baseline file (from the repo root)
cd ..
bash scripts/update_wasm_baseline.sh

# 3. Commit
git add Stellar-contracts-v1/wasm-size-baseline.json
git commit -m "chore: update WASM size baseline"
```

> **First-time setup**: The baseline ships with zeros so the first CI run reports sizes without failing. Copy the byte values from the Job Summary into `wasm-size-baseline.json` (or run `update_wasm_baseline.sh` locally) and commit them before relying on regression detection.

## DEX / AMM

Pool creation against Soroswap or another Stellar AMM is **not** included here; seed liquidity off-chain after deploying both tokens.

## Proof of reserve

wPi minting is admin/relayer-gated. Short-term **proof of reserve** is an off-chain signed attestation process (not an on-chain mint guard yet):

| Resource | Location |
|----------|----------|
| Process & ops | [`docs/proof-of-reserve.md`](../docs/proof-of-reserve.md) |
| On-chain oracle design | [`docs/design/on-chain-reserve-oracle.md`](../docs/design/on-chain-reserve-oracle.md) |
| Attestor CLI | `scripts/por/attest.mjs`, `scripts/por/verify.mjs` |
| Public feed | [`attestations/latest.json`](../attestations/latest.json) (demo until production cadence) |

```bash
# From repo root
node scripts/por/verify.mjs attestations/latest.json
```
