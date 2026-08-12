# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in the DvP swap program, please report
it privately. Do **not** open a public issue, PR, or disclose it in any public
forum before it has been addressed.

- Email: **security@solana.org**
- Use GitHub's [private vulnerability reporting](https://github.com/solana-foundation/dvp/security/advisories/new)
  to open a confidential advisory.

Please include:

- A description of the vulnerability and its impact.
- Steps to reproduce, a proof of concept, or affected code paths.
- Any relevant transaction signatures, program addresses, or logs.

We will acknowledge receipt, investigate, and coordinate a fix and disclosure
timeline with you. Please give us a reasonable window to remediate before any
public disclosure.

## Scope

- **Program ID:** `dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq`
- **Source:** the `program/` crate in this repository.

## Verified Builds

The deployed program is reproducible from source. To confirm the on-chain
bytecode matches this repository:

```bash
solana-verify verify-from-repo https://github.com/solana-foundation/dvp \
  --program-id dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq \
  --library-name dvp_swap_program
```

## Audits

See [AUDIT_STATUS.md](AUDIT_STATUS.md) for audit history and status.
