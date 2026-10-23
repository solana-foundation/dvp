import {
  ExtensionType,
  findExtraAccountMetaListPda,
  getUpdateTransferHookInstruction,
  getEnableMemoTransfersInstruction,
  getReallocateInstruction,
} from "@solana-program/token-2022";
import {
  AccountRole,
  generateKeyPairSigner,
  address,
  type Address,
} from "@solana/kit";
import { getTransferSolInstruction } from "@solana-program/system";
import assert from "node:assert/strict";
import { test } from "node:test";
import { resolve } from "node:path";
import {
  PlannedTransaction,
  readEscrowBalance,
  resolveConfidentialHookAccounts,
  settleSession,
} from "../../confidential";
import { TestContext, execute, send } from "./context";
import { createAndFund, fixture } from "./utils";

// "prefunded" is a System-owned validation PDA with no data, which anyone can create.
for (const validationState of ["present", "absent", "prefunded"] as const) {
  test(`resolve confidential hooks and settle with a required recipient memo (validation=${validationState})`, async () => {
    const context = new TestContext(),
      f = await fixture(context, 1, { hook: true });
    try {
      const hook = {
        rpc: context.rpc,
        transferHookProgramAddress: address(f.hookProgram) as Address,
        mint: f.common.mintB,
      };
      if (validationState !== "present") {
        hook.transferHookProgramAddress = (
          await generateKeyPairSigner()
        ).address;
        context.addProgramFromFile(
          hook.transferHookProgramAddress,
          resolve(
            process.env.SBF_OUT_PATH ?? "target/deploy",
            "transfer_hook_fixture.so",
          ),
        );
        await send(
          context,
          f.config,
          new PlannedTransaction([
            getUpdateTransferHookInstruction({
              mint: f.common.mintB,
              authority: f.payer,
              programId: hook.transferHookProgramAddress,
            }),
          ]),
        );
        const [validation] = await findExtraAccountMetaListPda(
          { mint: hook.mint },
          { programAddress: hook.transferHookProgramAddress },
        );
        assert.equal(context.account(validation), undefined);
        if (validationState === "prefunded") {
          await send(
            context,
            f.config,
            new PlannedTransaction([
              getTransferSolInstruction({
                source: f.payer,
                destination: validation,
                amount: context.minimumBalanceForRentExemption(0n),
              }),
            ]),
          );
          assert.equal(context.account(validation)?.data.length, 0);
        }
      }
      const fundingExtras = await resolveConfidentialHookAccounts({
        ...hook,
        source: f.common.userBAtaB,
        destination: f.common.dvpAtaB,
        owner: f.userB.address,
      });
      await createAndFund(context, f, f.amount, fundingExtras);
      await send(
        context,
        f.config,
        new PlannedTransaction([
          getReallocateInstruction({
            token: address(f.recipient),
            owner: f.userA,
            payer: f.payer,
            newExtensionTypes: [ExtensionType.MemoTransfer],
          }),
          getEnableMemoTransfersInstruction({
            token: address(f.recipient),
            owner: f.userA,
          }),
        ]),
      );

      const extras = await resolveConfidentialHookAccounts({
        ...hook,
        source: f.common.dvpAtaB,
        destination: address(f.recipient),
        owner: f.common.swapDvp,
      });
      if (validationState !== "present")
        assert.deepEqual(extras, [
          {
            address: hook.transferHookProgramAddress,
            role: AccountRole.READONLY,
          },
        ]);
      const request = {
        source: f.source(),
        swap: f.swap(),
        expectedAmountB: f.amount,
        recipient: context.account(address(f.recipient))!,
        mint: context.account(f.common.mintB)!,
        surplusRecipient: context.account(f.common.userBAtaB)!,
      };
      await assert.rejects(
        settleSession({ ...f.config, format: 0 }, f.settle, request, {
          legB: extras,
        }),
        { name: "ConfidentialError", code: "HookedSettleRequiresV1" },
      );
      const history = await execute(
        context,
        f.config,
        await settleSession(f.config, f.settle, request, { legB: extras }),
      );
      const inner = history.flatMap((tx) =>
        [...tx.innerInstructions.values()].flat(),
      );
      assert(
        inner.some(
          (ix) => ix.programAddress === hook.transferHookProgramAddress,
        ),
      );
      assert(inner.some((ix) => ix.programAddress === f.common.memoProgram));
      assert.equal(
        readEscrowBalance(f.state(address(f.recipient)), 0n, f.recipientKeys)
          .pending,
        f.amount,
      );
    } finally {
      f.close();
    }
  });
}

test("hook resolver preserves malformed validation and fetch errors", async () => {
  const context = new TestContext();
  const f = await fixture(context, 1, { hook: true });
  try {
    const input = {
      rpc: context.rpc,
      transferHookProgramAddress: address(f.hookProgram) as Address,
      mint: f.common.mintB,
      source: f.common.dvpAtaB,
      destination: address(f.recipient),
      owner: f.common.swapDvp,
    };
    const [validation] = await findExtraAccountMetaListPda(
      { mint: input.mint },
      { programAddress: input.transferHookProgramAddress },
    );
    context.setAccount({
      ...context.account(validation)!,
      address: validation,
      data: new Uint8Array([1]),
    });
    await assert.rejects(resolveConfidentialHookAccounts(input));
    const failure = new Error("hook account fetch failed");
    const rpc = {
      getAccountInfo: () => ({
        send: async () => {
          throw failure;
        },
      }),
    } as unknown as typeof context.rpc;
    await assert.rejects(
      resolveConfidentialHookAccounts({ ...input, rpc }),
      (error) => error === failure,
    );
  } finally {
    f.close();
  }
});
