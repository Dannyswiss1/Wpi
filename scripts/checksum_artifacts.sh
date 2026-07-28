#!/usr/bin/env bash
# checksum_artifacts.sh
# Generates release checksums and system info for reproducible verification.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASM_DIR="${REPO_ROOT}/Stellar-contracts-v1/target/wasm32-unknown-unknown/release"
OUTPUT_FILE="${WASM_DIR}/SHA256SUMS"

CONTRACTS=("wpi_token" "mock_amm")

echo "=== Generating Artifact SHA-256 Checksums ==="
echo ""

# Ensure release target directory exists
if [[ ! -d "$WASM_DIR" ]]; then
  echo "ERROR: Release build target directory not found: ${WASM_DIR}" >&2
  echo "Please build the contracts first using 'make build'." >&2
  exit 1
fi

# Verify wasm files exist
for name in "${CONTRACTS[@]}"; do
  WASM_FILE="${WASM_DIR}/${name}.wasm"
  if [[ ! -f "$WASM_FILE" ]]; then
    echo "ERROR: ${name}.wasm not found in ${WASM_DIR}. Please run 'make build' first." >&2
    exit 1
  fi
done

# Write SHA256 checksums to file
if command -v sha256sum &>/dev/null; then
  (cd "$WASM_DIR" && sha256sum "${CONTRACTS[@]/%/.wasm}" > "SHA256SUMS")
elif command -v shasum &>/dev/null; then
  (cd "$WASM_DIR" && shasum -a 256 "${CONTRACTS[@]/%/.wasm}" > "SHA256SUMS")
else
  # Minimal fallback or error
  echo "ERROR: Neither 'sha256sum' nor 'shasum' utility was found. Cannot generate checksum file." >&2
  exit 1
fi

echo "Checksums written to: ${OUTPUT_FILE}"
echo "------------------------------------"
cat "$OUTPUT_FILE"
echo "------------------------------------"

# Retrieve git commit details
GIT_REV="unknown"
if command -v git &>/dev/null && git rev-parse --is-inside-work-tree &>/dev/null; then
  GIT_REV=$(git rev-parse HEAD)
fi

# Retrieve compiler version
RUST_VER="unknown"
if command -v rustc &>/dev/null; then
  RUST_VER=$(rustc --version)
fi

echo ""
echo "=== Release Verification Metadata ==="
echo "Compiler Version: ${RUST_VER}"
echo "Build Target:     wasm32-unknown-unknown"
echo "Source Revision:  ${GIT_REV}"
echo "Release Date:     $(date -u +"%Y-%m-%dT%H:%M:%SZ" 2>/dev/null || date)"
echo "------------------------------------"
