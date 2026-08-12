# Program Upgrade Mechanism

This document describes how the DvP swap program is deployed and upgraded, per
SDLC Section 3.5.1.

- **Program ID:** `dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq` (same on devnet and
  mainnet; the address is set by the vanity program keypair, not the authority).
- **Program keypair:** managed secret, never committed (gitignored `keys/`).
  Needed only to create the program account on first deploy per cluster.
- **Deployer keypair:** fee payer and buffer writer, loaded in CI from Doppler.
  Holds upgrade authority on devnet and through mainnet launch. Never a multisig
  member.

## Governing rule

Upgrade authority moves to the Squads multisig only once the program is final:
deployed, verified on-chain, IDL published, and the last version before
go-to-market. Until then the deployer keypair retains authority so fixes ship
without a multisig round-trip.

## First deployment (net-new program)

The first deploy on a cluster creates the program account and requires the
program keypair to sign. It is done manually by the deployer keypair, not
through CI:

```bash
solana program deploy target/deploy/dvp_swap_program.so \
  --program-id keys/dvp_swap_program-keypair.json \
  --upgrade-authority keys/deployer.json \
  --url <RPC_URL> \
  --with-compute-unit-price <PRIORITY_FEE>
```

Then verify on-chain and publish the IDL:

```bash
solana-verify verify-from-repo https://github.com/solana-foundation/dvp \
  --program-id dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq \
  --library-name dvp_swap_program --remote

program-metadata write idl dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq \
  idl/dvp_swap_program.json --keypair keys/deployer.json --rpc <RPC_URL>
```

## Subsequent upgrades (CI)

Run the `Release` GitHub Actions workflow (`.github/workflows/release.yml`) with
the target network. It loads the deployer keypair from Doppler over OIDC, builds
the verified program, and:

- **devnet** — writes a program buffer and upgrades the program directly with the
  deployer keypair, then uploads the IDL via program-metadata.
- **mainnet** — writes the program and IDL buffers and transfers both buffer
  authorities to the Squads vault. CI does not execute the upgrade. A human then
  creates the Squads upgrade proposal from the buffers listed in the run summary.
  The CI keypair is fee payer and buffer writer only; it is not a Squads member.

## Verifying a proposed mainnet upgrade

Before approving a Squads proposal, each member verifies the buffer bytecode
matches source:

```bash
git checkout <COMMIT_HASH>
solana-verify build --library-name dvp_swap_program
solana-verify get-executable-hash target/deploy/dvp_swap_program.so
solana-verify get-buffer-hash -u <RPC_URL> <BUFFER_ADDRESS>
```

If the hashes match, the buffer contains exactly the bytecode from that commit.
If not, reject and investigate.

## Verifying the deployed program

```bash
solana-verify verify-from-repo https://github.com/solana-foundation/dvp \
  --program-id dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq \
  --library-name dvp_swap_program
```

## References

- [Solana Verified Builds](https://solana.com/docs/programs/verified-builds)
- [Squads Protocol](https://squads.xyz/)
- [Program Metadata](https://github.com/solana-program/program-metadata)
