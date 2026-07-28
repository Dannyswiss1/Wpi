# Design: Admin role separation for wpi-token

**Status:** Implemented
**Parent:** [Issue #5](https://github.com/privexlabs/Wpi/issues/5) — Single admin key controls mint, burn, pause, and admin transfer

## Problem

Before this change, one `Address` (`BRIDGE_STELLAR_ADMIN_SECRET_KEY` per the
`Stellar-contracts-v1` README) had unilateral mint, burn, pause, and
admin-transfer power over `wpi-token`. If that key were compromised, an
attacker could mint unlimited wPi, and no independent role could stop them
short of an off-chain response.

`wpi-token` already had one precedent for splitting privileged access: the
`VolumeLimitAdmin` role (Issue #26), which independently owns
`configure_volume_limits`/`override_volume_limit` and can only be rotated by
itself, not by the bridge admin. This design extends that same pattern to the
remaining admin powers.

## Roles (target architecture, implemented)

| Role | Storage key | Gates | Rotated by |
|---|---|---|---|
| Admin | `Admin` | `propose_admin`/`accept_admin` (two-step), `upgrade` | Two-step: current admin proposes, proposed admin accepts |
| Minter | `Minter` | `mint`, `mint_from_deposit`, `burn` | Current minter only, via `set_minter` |
| Pauser | `Pauser` | `set_paused` | Current pauser only, via `set_pauser` |
| Volume-limit admin | `VolumeLimitAdmin` | `configure_volume_limits`, `override_volume_limit`, `set_volume_limit_admin` | Current volume-limit admin only (pre-existing, unchanged) |

All four roles default to the address passed to `initialize` and must be
rotated independently by the deployer before routing real bridge traffic,
exactly like the existing `VolumeLimitAdmin` convention.

**Deliberate design choice — self-rotation, not admin-controlled:** `set_minter`
and `set_pauser` are gated by the *current* minter/pauser, not by `Admin`.
If `Admin` could reassign the minter or pauser at will, it would still be a
single point of failure: a compromised admin could simply install a new
minter it controls. Making each role self-rotating means handing it to a
multisig is a one-way door — `Admin` retains upgrade and admin-transfer
authority, but never mint, burn, or pause authority directly. Splitting
`Pauser` from `Minter` also means a compromised minter can be halted by an
independent guardian instead of by the same key that is misbehaving.

## Admin (and every role) can already be a contract address

Soroban's `Address` type is uniform: it represents either a classic Stellar
account or a contract implementing the custom-account interface
(`__check_auth`), and `require_auth()` resolves either transparently. Nothing
in `wpi-token` inspects which kind of address a role holds — every gate is
`some_role_address.require_auth()`. That means:

- A Soroban multisig/policy contract can already be deployed and set as any
  of these four roles with **no contract changes**.
- Protocol 27 ("Zipper," live on mainnet since 2026-07-10) added CAP-0071
  native authentication delegation and `SOROBAN_CREDENTIALS_ADDRESS_V2`. Per
  the issue's own recommendation, we evaluated this instead of building a
  bespoke multisig contract: **we did not add a custom multisig contract to
  this repo.** A classic Stellar multisig account (weighted signers +
  thresholds) or any CAP-0071-delegated account can be used directly as the
  `Address` for any role, using the network's audited primitive rather than
  new, unaudited contract code.
- `wpi-token/src/test.rs::every_role_can_be_a_contract_address_not_only_an_eoa`
  proves this by registering a second contract instance and using its
  address as every role: `initialize`, `mint`, and the role getters all
  work identically whether the stored address is an EOA or a contract.

## Migration plan for existing testnet deployments

Deployments initialized before this change never wrote the `Minter`/`Pauser`
storage keys. `read_minter`/`read_pauser` fall back to the live `Admin`
address when those keys are absent — the same fallback `read_volume_limit_admin`
already uses for pre-Issue-26 deployments — so upgrading in place does not
interrupt bridge traffic.

1. **Upgrade the WASM.** The existing admin calls `upgrade` with the new
   `wpi_token.wasm` hash, as it already can today. No storage migration step
   is required: `minter()`/`pauser()` immediately report the current admin
   address via the fallback.
2. **Verify the fallback.** Call the read-only `minter()` and `pauser()`
   functions and confirm they return the current admin address.
3. **Rotate each role independently**, in any order, to its intended holder
   (a dedicated relayer key, a multisig, or a delegated account):
   ```bash
   stellar contract invoke --id "$WPI_CONTRACT_ID" --source "$ADMIN_IDENTITY" \
     --network testnet -- set_minter --new_minter "$BRIDGE_OPS_ADDRESS"

   stellar contract invoke --id "$WPI_CONTRACT_ID" --source "$ADMIN_IDENTITY" \
     --network testnet -- set_pauser --new_pauser "$GUARDIAN_ADDRESS"
   ```
   `VolumeLimitAdmin` rotation is unchanged (see main README).
4. **Rotate `Admin` last**, once minter/pauser/volume-limit-admin already
   point at their intended holders, using the existing two-step
   `propose_admin`/`accept_admin` flow. Rotating admin last means the
   deployer never loses the ability to fix a mis-rotated minter/pauser via a
   contract `upgrade` mid-migration.
5. **Confirm** via `admin()`, `minter()`, `pauser()`, and
   `volume_limit_admin()` that no address still holds more than its intended
   role, and that the relayer's `BRIDGE_STELLAR_ADMIN_SECRET_KEY` only needs
   the `Minter` role to keep operating `mint_from_deposit`.

No changes to the relayer service are required: it only ever calls
`mint_from_deposit`, which is gated by `Minter`, and the fallback keeps that
working through the migration window described above.

## Trust model

| Actor | Can do | Cannot do |
|---|---|---|
| Minter | Mint, mint-from-deposit, burn (bridge redemption) | Pause, reconfigure volume limits, transfer admin, upgrade |
| Pauser | Halt/resume all token state changes (subject to the volume-limit circuit breaker) | Mint, burn, reconfigure volume limits, transfer admin, upgrade |
| Volume-limit admin | Configure/override the mint & burn circuit breaker (unchanged from Issue #26) | Mint, burn, pause directly, transfer admin, upgrade |
| Admin | Propose/accept admin transfer, upgrade the contract | Mint, burn, pause, or reclaim minter/pauser without their cooperation |

A single compromised key now controls at most one of: minting, pausing,
volume-limit policy, or upgrade/admin-transfer — never all four.

## Testing

See `wpi-token/src/test.rs`:

- `minter_role_is_independent_from_bridge_admin`,
  `admin_cannot_authenticate_mint_after_minter_rotation`,
  `admin_cannot_reclaim_minter_role`
- `pauser_role_is_independent_from_bridge_admin_and_minter`,
  `admin_cannot_authenticate_set_paused_after_pauser_rotation`
- `upgraded_deployment_without_stored_minter_or_pauser_falls_back_to_admin`
  (migration fallback)
- `every_role_can_be_a_contract_address_not_only_an_eoa` (contract-address /
  multisig admin)

## Acceptance criteria (Issue #5)

- [x] Admin `Address` can be a contract address (multisig or delegated
      account), not just an EOA — verified with a test; no contract change
      was needed since `Address`/`require_auth()` already support this.
- [x] Role separation (minter, pauser, upgrader/admin) documented here and
      enforced on-chain, not just by convention.
- [x] Migration plan written for moving existing testnet deployments to the
      new admin model (above).
