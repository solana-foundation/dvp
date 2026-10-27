set shell := ["bash", "-euo", "pipefail", "-c"]

# Pin the SBF compiler used by Agave 4.2.2's cargo-build-sbf 4.1.0.
sbf_tools_version := env("SBF_TOOLS_VERSION", "v1.54")

default: build

# Install JS deps (codama renderers, tsx, etc.)
install:
    pnpm install

# External programs are downloaded explicitly and checked before test runs.
fetch-test-fixtures:
    bash scripts/fetch-test-fixtures.sh

check-test-fixtures:
    bash scripts/fetch-test-fixtures.sh --check

# Build the on-chain program. Regenerates clients first so the workspace
# Rust client crate is up to date before cargo-build-sbf compiles the program.
# cargo-build-sbf must run from program/ so it only sees the on-chain crate
# (the client crate has host-only deps that fail under solana CLI's stricter
# sbf target).
[doc("Generate clients, then build the program and test fixtures")]
build: generate-clients && build-sbf build-hook-fixture build-smart-wallet-fixture build-confidential-fixture

# CI receives generated clients from the generation job before this step.
build-sbf:
    cd program && cargo-build-sbf --tools-version {{ sbf_tools_version }} -- --locked

# Pre-deploy guard: cargo-build-sbf writes a random target/deploy/*-keypair.json
# that does NOT match declare_id!, so deploying with it (or without an explicit
# --program-id) publishes to the wrong address. Run this before `solana program
# deploy` with the real deploy keypair in place. Not wired into `build`/CI: there
# the keypair is a throwaway and would never match. Expected ID is read from the
# generated IDL (single source of truth, derived from declare_id!).
[doc("Pre-deploy: check the deploy keypair matches the declared program ID")]
verify-program-id:
    #!/usr/bin/env bash
    set -euo pipefail
    expected=$(grep -o '"publicKey": *"[^"]*"' idl/dvp_swap_program.json | head -1 | sed 's/.*"\([^"]*\)"$/\1/')
    actual=$(solana-keygen pubkey target/deploy/dvp_swap_program-keypair.json)
    if [ "$expected" != "$actual" ]; then
        echo "ERROR: deploy keypair $actual does not match program ID $expected"
        exit 1
    fi
    echo "OK: deploy keypair matches program ID $expected"

# Build the no-op transfer-hook program used only by integration tests.
# Loaded into LiteSVM so hook-bearing Token-2022 mints can be exercised
# end-to-end. The .so lands in the workspace target/deploy/ alongside
# the swap program's .so.
[doc("Build the transfer-hook fixture for integration tests")]
build-hook-fixture:
    cd tests/transfer-hook-fixture && cargo-build-sbf --tools-version {{ sbf_tools_version }} -- --locked

# Build the smart-wallet fixture used by integration tests to model a
# Squads-style vault party (signs via CPI). The .so lands in the
# workspace target/deploy/ alongside the swap program's .so.
[doc("Build the smart-wallet fixture for integration tests")]
build-smart-wallet-fixture:
    cd tests/smart-wallet-fixture && cargo-build-sbf --tools-version {{ sbf_tools_version }} -- --locked

# Test-only PDA driver for the shared confidential transfer helpers.
[doc("Build the confidential transfer fixture for integration tests")]
build-confidential-fixture:
    cd tests/confidential-transfer-fixture && cargo-build-sbf --tools-version {{ sbf_tools_version }} -- --locked

# Generate the Codama IDL from the program's annotations.
generate-idl:
    @echo "Generating IDL..."
    pnpm run generate-idl

# Generate Rust + TypeScript clients from the IDL.
generate-clients: generate-idl
    @echo "Generating clients..."
    pnpm run generate-clients

# Format and lint.
fmt:
    cargo fmt --all
    @cd program && cargo clippy --all-targets -- -D warnings
    @cd tests/integration-tests && cargo clippy --all-targets -- -D warnings
    pnpm format

fmt-check: generate-clients
    cargo fmt --all --check
    pnpm format:check

# Clippy on integration-tests needs the .so files it include_bytes!.
lint-check: build check-test-fixtures
    cargo check -p dvp-swap-program-client --locked --no-default-features
    cargo check -p dvp-swap-program-client --locked --no-default-features --features fetch
    cargo check -p dvp-swap-program-client --locked --no-default-features --features confidential
    cargo check -p dvp-swap-program-client --locked --no-default-features --features confidential,fetch --example confidential
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

check-generated: generate-clients
    #!/usr/bin/env bash
    set -euo pipefail
    paths=(idl/dvp_swap_program.json clients/rust clients/typescript)
    if [[ -n "$(git status --porcelain --untracked-files=all -- "${paths[@]}")" ]]; then
        echo "Error: generated files are out of date. Run: just generate-clients"
        git status --short -- "${paths[@]}"
        git diff -- "${paths[@]}"
        exit 1
    fi

# Unit tests: program and Rust client #[cfg(test)] modules + JS client tests
# that need no program build.
unit-test: generate-clients
    @echo "Running unit tests for swap program..."
    pnpm test:unit
    @cd program && cargo test
    cargo test -p dvp-swap-program-client --all-features --lib

# Integration tests: LiteSVM Rust suites and the full TypeScript client suite.
integration-test-no-build: check-test-fixtures && typescript-test
    @echo "Running integration tests for swap program..."
    @cd tests/integration-tests && cargo test -- --nocapture
    cargo test -p dvp-swap-program-client --all-features --test integration -- --nocapture

typescript-test: check-test-fixtures
    pnpm test

integration-test: build integration-test-no-build

all-test: build && unit-test integration-test-no-build

# Run unit tests with coverage
unit-coverage:
    @echo "Running unit tests with coverage..."
    @mkdir -p coverage
    @cd program && cargo llvm-cov --lib --tests --lcov --output-path ../coverage/coverage-dvp-swap-unit.lcov

# Generate HTML coverage report
coverage-html:
    @echo "Generating HTML coverage report..."
    @mkdir -p coverage
    @cd program && cargo llvm-cov --html --output-dir ../coverage/coverage-dvp-swap-html

# Run all coverage tasks
all-coverage: unit-coverage coverage-html
