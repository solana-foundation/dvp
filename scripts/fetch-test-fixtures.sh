#!/usr/bin/env bash
# Download the pinned external programs loaded by integration tests.
set -euo pipefail
fixtures="$(cd "$(dirname "$0")/../tests/fixtures" && pwd)"

if [[ "${1:-}" == "--check" ]]; then
    cd "$fixtures"
    shasum -a 256 -c SHA256SUMS
    exit
fi

rpc="${1:-https://api.mainnet-beta.solana.com}"
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT
solana program dump --url "$rpc" TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb "$staging/spl_token_2022.so"
solana program dump --url "$rpc" recr1L3PCGKLbckBqMNcJhuuyU1zgo8nBhfLVsJNwr5 "$staging/spl_record.so"
(
    cd "$staging"
    shasum -a 256 -c "$fixtures/SHA256SUMS"
)
# Install only after both hashes match. An upstream upgrade must be reviewed.
cp "$staging/spl_token_2022.so" "$staging/spl_record.so" "$fixtures/"
