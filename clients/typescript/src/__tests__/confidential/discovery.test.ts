import { getCreateAccountInstruction } from "@solana-program/system";
import {
  generateKeyPairSigner,
  type GetProgramAccountsApi,
  type Rpc,
} from "@solana/kit";
import assert from "node:assert/strict";
import { test } from "node:test";
import { DVP_SWAP_PROGRAM_PROGRAM_ADDRESS } from "../../generated/programs/dvpSwapProgram";
import {
  CONFIDENTIAL_SWAP_DVP_ACCOUNT_SIZE,
  fetchSwapDvpAccounts,
  SWAP_DVP_ACCOUNT_SIZE,
} from "../../verify";
import { PlannedTransaction, createSession } from "../../confidential";
import { TestContext, execute, send } from "./context";
import { fixture } from "./utils";

test("discovery skips uninitialized program-owned accounts in both layouts", async () => {
  const context = new TestContext(),
    f = await fixture(context, 1);
  try {
    await execute(
      context,
      f.config,
      createSession(f.config, f.create, f.keys, f.amount),
    );
    const addresses = [f.common.swapDvp];
    // Anyone can assign the DvP owner through System CreateAccount, without invoking DvP.
    for (const space of [
      SWAP_DVP_ACCOUNT_SIZE,
      CONFIDENTIAL_SWAP_DVP_ACCOUNT_SIZE,
    ]) {
      const newAccount = await generateKeyPairSigner();
      await send(
        context,
        f.config,
        new PlannedTransaction([
          getCreateAccountInstruction({
            payer: f.payer,
            newAccount,
            space,
            lamports: context.minimumBalanceForRentExemption(BigInt(space)),
            programAddress: DVP_SWAP_PROGRAM_PROGRAM_ADDRESS,
          }),
        ]),
      );
      addresses.push(newAccount.address);
    }
    // Adapt only the RPC listing; every account above was created by a real transaction.
    const rpc = {
      getProgramAccounts: (
        owner: string,
        { filters }: { filters: { dataSize: bigint }[] },
      ) => ({
        send: async () =>
          addresses.flatMap((key) => {
            const raw = context.account(key)!;
            if (
              raw.programAddress !== owner ||
              raw.space !== filters![0]!.dataSize
            )
              return [];
            return [
              {
                pubkey: key,
                account: {
                  owner: raw.programAddress,
                  data: [Buffer.from(raw.data).toString("base64"), "base64"],
                  executable: raw.executable,
                  lamports: raw.lamports,
                  space: raw.space,
                },
              },
            ];
          }),
      }),
    } as unknown as Rpc<GetProgramAccountsApi>;
    const swaps = await fetchSwapDvpAccounts(rpc);
    assert.deepEqual(
      swaps.map((swap) => swap.address),
      [f.common.swapDvp],
    );
  } finally {
    f.close();
  }
});
