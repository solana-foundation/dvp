import assert from "node:assert/strict";
import { test } from "node:test";
import {
  AccountRole,
  generateKeyPairSigner,
  type Address,
  type Instruction,
} from "@solana/kit";
import { ZK_ELGAMAL_PROOF_PROGRAM_ADDRESS } from "@solana-program/zk-elgamal-proof";
import { ProofKind } from "../../confidential/transaction";
import { getCreateAccountInstruction } from "@solana-program/system";
import {
  getConfidentialTransferInstructionDataDecoder,
  getConfidentialTransferWithFeeInstructionDataEncoder,
  identifyToken2022Instruction,
  Token2022Instruction,
  TOKEN_2022_PROGRAM_ADDRESS,
} from "@solana-program/token-2022";
import {
  RECORD_PROGRAM_ADDRESS,
  RECORD_META_DATA_SIZE,
  getInitializeInstruction,
  getWriteInstruction,
} from "@solana-program/record";
import {
  BalanceHistory,
  PlannedTransaction,
  applySession,
  recoverEscrowBalance,
} from "../../confidential";
import { TestContext, send } from "./context";
import { fixture, createAndFund } from "./utils";

for (const format of [0, 1] as const) {
  test(`replay ignores writes to an unrelated pre-existing Record (v${format})`, async () => {
    const context = new TestContext();
    const f = await fixture(context, format);
    try {
      const record = await generateKeyPairSigner();
      const space = Number(RECORD_META_DATA_SIZE) + 1;
      await send(
        context,
        f.config,
        new PlannedTransaction([
          getCreateAccountInstruction({
            payer: f.config.payer,
            newAccount: record,
            lamports: context.minimumBalanceForRentExemption(BigInt(space)),
            space,
            programAddress: RECORD_PROGRAM_ADDRESS,
          }),
          getInitializeInstruction({
            recordAccount: record.address,
            authority: f.config.payer.address,
          }),
        ]),
      );
      const history = await createAndFund(context, f);
      const before = f.state(f.common.dvpAtaB);
      const apply = applySession(f.config, f.apply, {
        state: before,
        keys: f.keys,
      });
      const composed = await send(
        context,
        f.config,
        new PlannedTransaction([
          getWriteInstruction({
            recordAccount: record.address,
            authority: f.config.payer,
            offset: 0n,
            data: new Uint8Array([42]),
          }),
          ...apply.finalTransaction.instructions,
        ]),
      );
      const after = f.state(f.common.dvpAtaB);
      assert.deepEqual(after.availableBalance, before.availableBalance);
      assert.equal(after.pendingBalanceCreditCounter, 0n);
      assert.equal(context.account(record.address)!.data.at(-1), 42);
      const replay = new BalanceHistory(f.common.dvpAtaB);
      for (const tx of [...history, composed]) replay.push(tx);
      assert.equal(
        recoverEscrowBalance(after, 0n, f.keys, replay.events()).available,
        f.amount,
      );
      if (format === 0) {
        // Replay the real verified validity proof using external Record storage.
        // Range-proof storage alone is not required to reconstruct the balance.
        let proofBytes: Uint8Array | undefined;
        const withoutInitialization = history.map((tx) => ({
          ...tx,
          instructions: tx.instructions.flatMap((ix) => {
            if (
              ix.programAddress !== ZK_ELGAMAL_PROOF_PROGRAM_ADDRESS ||
              ix.data?.[0] !== ProofKind.GroupedValidity ||
              ix.data.length === 5
            )
              return [ix];
            assert.equal(proofBytes, undefined);
            proofBytes = new Uint8Array(
              Number(RECORD_META_DATA_SIZE) + ix.data.length - 1,
            );
            proofBytes.set(ix.data.slice(1), Number(RECORD_META_DATA_SIZE));
            const data = new Uint8Array(5);
            data[0] = ProofKind.GroupedValidity;
            new DataView(data.buffer).setUint32(
              1,
              Number(RECORD_META_DATA_SIZE),
              true,
            );
            return [
              getWriteInstruction({
                recordAccount: record.address,
                authority: f.payer,
                offset: 0n,
                data: ix.data.slice(1),
              }),
              {
                ...ix,
                data,
                accounts: [
                  { address: record.address, role: AccountRole.READONLY },
                  ...(ix.accounts ?? []),
                ],
              },
            ];
          }),
        }));
        assert(proofBytes);
        const missing = new BalanceHistory(f.common.dvpAtaB);
        assert.throws(
          () => {
            for (const tx of withoutInitialization) missing.push(tx);
          },
          { code: "IncompleteHistory" },
        );
        const resolved = new BalanceHistory(f.common.dvpAtaB);
        for (const tx of withoutInitialization)
          resolved.push(tx, (ix) =>
            ix.accounts?.[0]?.address === record.address
              ? proofBytes
              : undefined,
          );
        assert.equal(
          recoverEscrowBalance(after, 0n, f.keys, resolved.events()).available,
          f.amount,
        );
      }
    } finally {
      f.close();
    }
  });
}

test("replay keeps a validity context when a verification omits its authority", async () => {
  const context = new TestContext();
  const f = await fixture(context, 0);
  try {
    const history = await createAndFund(context, f);
    const state = f.state(f.common.dvpAtaB);
    const record = (await generateKeyPairSigner()).address;
    let forgedRecord: Uint8Array | undefined;
    // Without the authority account the proof program verifies and creates
    // nothing, so a forged proof naming the real context must not replace it.
    const poisons = [
      (ix: Instruction, forged: Uint8Array): Instruction => ({
        ...ix,
        data: forged,
        accounts: [ix.accounts![0]!],
      }),
      (ix: Instruction, forged: Uint8Array): Instruction => {
        forgedRecord = new Uint8Array(
          Number(RECORD_META_DATA_SIZE) + forged.length - 1,
        );
        forgedRecord.set(forged.slice(1), Number(RECORD_META_DATA_SIZE));
        const data = new Uint8Array(5);
        data[0] = ProofKind.GroupedValidity;
        new DataView(data.buffer).setUint32(
          1,
          Number(RECORD_META_DATA_SIZE),
          true,
        );
        return {
          ...ix,
          data,
          accounts: [
            { address: record, role: AccountRole.READONLY },
            ix.accounts![0]!,
          ],
        };
      },
    ];
    for (const poison of poisons) {
      let injected = 0;
      const poisoned = history.map((tx) => ({
        ...tx,
        instructions: tx.instructions.flatMap((ix) => {
          if (
            ix.programAddress !== ZK_ELGAMAL_PROOF_PROGRAM_ADDRESS ||
            ix.data?.[0] !== ProofKind.GroupedValidity ||
            ix.data.length === 5 ||
            (ix.accounts?.length ?? 0) < 2
          )
            return [ix];
          injected++;
          const forged = new Uint8Array(ix.data);
          // Corrupt the first grouped ciphertext after the three public keys.
          forged[1 + 3 * 32]! ^= 1;
          return [ix, poison(ix, forged)];
        }),
      }));
      assert(injected > 0);
      const replay = new BalanceHistory(f.common.dvpAtaB);
      for (const tx of poisoned)
        replay.push(tx, (ix) =>
          ix.accounts?.[0]?.address === record ? forgedRecord : undefined,
        );
      assert.equal(
        recoverEscrowBalance(state, 0n, f.keys, replay.events()).available,
        f.amount,
      );
    }
  } finally {
    f.close();
  }
});

test("replay treats TransferWithFee as Transfer in both directions", async () => {
  const context = new TestContext();
  const f = await fixture(context, 0);
  try {
    const history = await createAndFund(context, f);
    let transfer: Instruction | undefined;
    // On a mint without TransferFeeConfig, Token-2022 runs TransferWithFee
    // with the Transfer accounts and proofs, ignoring the fee offsets.
    const withFee = history.map((tx) => ({
      ...tx,
      instructions: tx.instructions.map((ix) => {
        if (
          ix.programAddress !== TOKEN_2022_PROGRAM_ADDRESS ||
          identifyToken2022Instruction(ix.data!) !==
            Token2022Instruction.ConfidentialTransfer
        )
          return ix;
        transfer = ix;
        const data = getConfidentialTransferInstructionDataDecoder().decode(
          ix.data!,
        );
        return {
          ...ix,
          data: getConfidentialTransferWithFeeInstructionDataEncoder().encode({
            ...data,
            transferAmountCiphertextValidityProofInstructionOffset:
              data.ciphertextValidityProofInstructionOffset,
            feeSigmaProofInstructionOffset: 0,
            feeCiphertextValidityProofInstructionOffset: 0,
          }),
        };
      }),
    }));
    assert(transfer);
    const replay = (escrow: Address, txs: typeof history) => {
      const result = new BalanceHistory(escrow);
      for (const tx of txs) result.push(tx);
      return result.events();
    };
    for (const escrow of [transfer.accounts![0]!, transfer.accounts![2]!]) {
      const expected = replay(escrow.address, history);
      assert(expected.some((e) => e.kind === "credit" || e.kind === "debit"));
      assert.deepEqual(replay(escrow.address, withFee), expected);
    }
  } finally {
    f.close();
  }
});
