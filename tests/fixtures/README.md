# Integration-test programs

Tests use real Token-2022 and SPL Record binaries from Solana mainnet-beta,
loaded into LiteSVM with `add_program`. The SHA-256 hashes in `SHA256SUMS`
pin the exact bytes, including program-dump padding. These match the binaries
used by the confidential-transfer spike.

| File | Mainnet program |
| --- | --- |
| `spl_token_2022.so` | `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb` |
| `spl_record.so` | `recr1L3PCGKLbckBqMNcJhuuyU1zgo8nBhfLVsJNwr5` |

From the repository root:

```sh
make fetch-test-fixtures
make check-test-fixtures
make all-test
```

The download script also accepts an RPC URL:

```sh
bash scripts/fetch-test-fixtures.sh https://api.mainnet-beta.solana.com
```

The `.so` files are ignored by Git. After an on-chain upgrade, downloading the
new binary fails the hash check and preserves existing local fixtures. Use an
archived copy matching `SHA256SUMS`, or review the new binary and regression
results before deliberately updating the hashes. Never accept new hashes just
to make setup pass. A supplied archive can be verified with
`make check-test-fixtures` without RPC access.

The DvP program and its transfer-hook and smart-wallet fixtures are built from
this checkout by `make build`. The ZK ElGamal proof program is the LiteSVM 0.16
builtin. The Record fixture is retained for later CT tests. Existing tests
enforce the legacy transaction wire-size limit explicitly, since LiteSVM
execution alone does not enforce that limit.

The pinned Token-2022 binary differs from the old bundled fixture: a
`TransferChecked` with a mint owned by another token program fails with
`IncorrectProgramId`. The injected cross-program mint recreation tests now
assert that `RejectDvp` and `RecoverDvp` fail atomically in this state. They
do not prove that this adversarial state is reachable through real token
instructions. The old tests' guarantee that these refunds always succeed
does not hold for these bytes. Separate empty-leg tests model mint A recreated
under legacy SPL with its old escrow empty: Reject skips the leg A transfer,
refunds B and closes the trade; user_b can also Reclaim B independently. These
tests cover the refund behavior without proving a full close-and-recreate flow.
