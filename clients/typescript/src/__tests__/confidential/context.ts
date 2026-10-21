import { RECORD_PROGRAM_ADDRESS } from "@solana-program/record";
import { TOKEN_2022_PROGRAM_ADDRESS } from "@solana-program/token-2022";
import {
  address,
  getBase58Decoder,
  getBase64EncodedWireTransaction,
  getCompiledTransactionMessageDecoder,
  getTransactionEncoder,
  getTransactionSizeLimit,
  type Address,
  type Base58EncodedBytes,
  type EncodedAccount,
  type GetAccountInfoApi,
  type GetLatestBlockhashApi,
  type GetMultipleAccountsApi,
  type GetMinimumBalanceForRentExemptionApi,
  type Rpc,
} from "@solana/kit";
import { FailedTransactionMetadata, LiteSVM } from "litesvm";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { DVP_SWAP_PROGRAM_PROGRAM_ADDRESS } from "../..";
import {
  executedTransactionFromRpc,
  type ExecutedTransaction,
  type PlannedTransaction,
  type RpcTransaction,
  type SessionConfig,
  type TransactionSession,
} from "../../confidential";

// Same no-op hook program as the Rust integration fixtures.
export const HOOK_PROGRAM = address(
  "HookqJupt6Khm8s8jB3p93NkhPoiAg2M7vkEhkS15CtC",
);

/** Test-only runtime and account transport. Client helpers never import this module. */
export class TestContext extends LiteSVM {
  constructor(programAddress: Address = DVP_SWAP_PROGRAM_PROGRAM_ADDRESS) {
    super();
    const deploy = process.env.SBF_OUT_PATH ?? "target/deploy";
    this.addProgramFromFile(
      programAddress,
      resolve(deploy, "dvp_swap_program.so"),
    );
    this.addProgramFromFile(
      HOOK_PROGRAM,
      resolve(deploy, "transfer_hook_fixture.so"),
    );
    const checksums = readFileSync("tests/fixtures/SHA256SUMS", "utf8");
    for (const [program, name] of [
      [TOKEN_2022_PROGRAM_ADDRESS, "spl_token_2022.so"],
      [RECORD_PROGRAM_ADDRESS, "spl_record.so"],
    ] as const) {
      const bytes = readFileSync(resolve("tests/fixtures", name));
      const expected = checksums
        .split("\n")
        .find((line) => line.endsWith(`  ${name}`))
        ?.split("  ")[0];
      assert.equal(
        createHash("sha256").update(bytes).digest("hex"),
        expected,
        name,
      );
      this.addProgram(program, bytes);
    }
    const clock = this.getClock();
    clock.unixTimestamp = BigInt(Math.floor(Date.now() / 1000));
    this.setClock(clock);
  }

  account(key: Address): EncodedAccount | undefined {
    const value = this.getAccount(key);
    return value.exists ? value : undefined;
  }

  advanceClock(seconds: bigint) {
    const clock = this.getClock();
    clock.unixTimestamp += seconds;
    this.setClock(clock);
  }

  // Only the SDK's account/rent reads are adapted; instructions execute in LiteSVM.
  readonly rpc = {
    getMultipleAccounts: (keys: readonly Address[]) => ({
      send: async () => ({
        context: { slot: this.getClock().slot },
        value: await Promise.all(
          keys.map(
            async (key) =>
              (
                await this.rpc
                  .getAccountInfo(key, { encoding: "base64" })
                  .send()
              ).value,
          ),
        ),
      }),
    }),
    getMinimumBalanceForRentExemption: (space: bigint) => ({
      send: async () => this.minimumBalanceForRentExemption(space),
    }),
    // Each example send asks for a blockhash; a fresh one keeps signatures unique.
    getLatestBlockhash: () => ({
      send: async () => {
        this.expireBlockhash();
        return {
          context: { slot: this.getClock().slot },
          value: {
            blockhash: this.latestBlockhash(),
            lastValidBlockHeight: 1_000_000n,
          },
        };
      },
    }),
    getAccountInfo: (key: Address) => ({
      send: async () => {
        const raw = this.account(key);
        return {
          context: { slot: this.getClock().slot },
          value: raw
            ? {
                owner: raw.programAddress,
                data: [Buffer.from(raw.data).toString("base64"), "base64"],
                executable: raw.executable,
                lamports: raw.lamports,
                space: BigInt(raw.data.length),
              }
            : null,
        };
      },
    }),
  } as Rpc<
    GetAccountInfoApi &
      GetLatestBlockhashApi &
      GetMultipleAccountsApi &
      GetMinimumBalanceForRentExemptionApi
  >;
}

export async function send(
  context: TestContext,
  config: SessionConfig,
  plan: PlannedTransaction,
): Promise<ExecutedTransaction> {
  context.expireBlockhash();
  const transaction = await plan.sign(config, {
    blockhash: context.latestBlockhash(),
    lastValidBlockHeight: 1_000_000n,
  });
  const wire = getTransactionEncoder().encode(transaction);
  assert(wire.length <= getTransactionSizeLimit(transaction));
  const result = context.sendTransaction(transaction);
  assert(!(result instanceof FailedTransactionMetadata), result.toString());

  // Reconstruct the history fields from the executed message and actual CPI metadata.
  const message = getCompiledTransactionMessageDecoder().decode(
    transaction.messageBytes,
  );
  const loadedAddresses: { writable: Address[]; readonly: Address[] } = {
    writable: [],
    readonly: [],
  };
  if (message.version === 0) {
    for (const lookup of message.addressTableLookups ?? []) {
      const addresses = config.lookupTables?.[lookup.lookupTableAddress];
      assert(addresses, "Executed lookup table must be available");
      loadedAddresses.writable.push(
        ...lookup.writableIndexes.map((index) => addresses[index]!),
      );
      loadedAddresses.readonly.push(
        ...lookup.readonlyIndexes.map((index) => addresses[index]!),
      );
    }
  }
  const rpc: RpcTransaction = {
    transaction: [getBase64EncodedWireTransaction(transaction), "base64"],
    meta: {
      err: null,
      loadedAddresses,
      innerInstructions: result.innerInstructions().map((group, index) => ({
        index,
        instructions: group.map((inner) => {
          const ix = inner.instruction();
          return {
            programIdIndex: ix.programIdIndex(),
            accounts: [...ix.accounts()],
            data: getBase58Decoder().decode(ix.data()) as Base58EncodedBytes,
          };
        }),
      })),
    },
  };
  return executedTransactionFromRpc(rpc);
}

/** Execute preparations in order, then final; verify successful cleanup. */
export async function execute(
  context: TestContext,
  config: SessionConfig,
  session: TransactionSession,
) {
  const history: ExecutedTransaction[] = [];
  for (const plan of [...session.preparation, session.finalTransaction])
    history.push(await send(context, config, plan));
  for (const ix of session.cleanup)
    assert.equal(context.account(ix.accounts![0]!.address), undefined);
  return history;
}
