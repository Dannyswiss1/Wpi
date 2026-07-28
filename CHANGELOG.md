# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-07-28

### Added
- **Contracts (`wpi-token`)**: Initial implementation of the Wrapped Pi (`wPi`) token contract on Stellar with admin-gated mint/burn and rolling volume-limit circuit breaker.
- **Contracts (`mock-amm`)**: Test AMM contract simulating wPi swaps against the real USDC Stellar Asset Contract (SAC).
- **Contracts (`soroban-token-common`)**: Shared balance, allowance, admin, and pause scaffolding.
- **Proof of Reserve (PoR)**: Tooling (`scripts/por/`) and schema (`attestations/schema.json`) for off-chain reserve attestation and signature verification.
- **Operations & Build**: Checked-in shell scripts for testnet and mainnet deployments (`deploy_testnet.sh`, `deploy_mainnet.sh`).
- **CI/CD**: Fully integrated workflows for cargo clippy, testing, dependency provenance verification, and WASM size regression tracking.
