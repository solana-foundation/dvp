import { hookExtras } from "../../confidential/lifecycle";
import { prepareTransfer } from "../../confidential/proofs";
import { SessionBuilder } from "../../confidential/transaction";
import {
  findAddressLookupTablePda,
  getCreateLookupTableInstructionAsync,
  getExtendLookupTableInstruction,
} from "@solana-program/address-lookup-table";
import { TOKEN_PROGRAM_ADDRESS } from "@solana-program/token";
import {
  TOKEN_2022_PROGRAM_ADDRESS,
  findAssociatedTokenPda,
  findExtraAccountMetaListPda,
  getConfidentialDepositInstruction,
  getConfidentialTransferInstruction,
  getCreateAssociatedTokenIdempotentInstructionAsync,
  getCreateMintInstructionPlan,
  getExtraAccountMetasEncoder,
  getMintToInstruction,
  getTokenDecoder,
  getTransferCheckedInstruction,
  type ExtensionArgs,
} from "@solana-program/token-2022";
import {
  getApplyConfidentialPendingBalanceInstructionFromToken,
  getCreateConfidentialTransferAccountInstructionPlan,
} from "@solana-program/token-2022/confidential";
import { ZK_ELGAMAL_PROOF_PROGRAM_ADDRESS } from "@solana-program/zk-elgamal-proof";
import {
  address,
  assertIsSingleInstructionPlan,
  flattenInstructionPlan,
  generateKeyPairSigner,
  getAddressDecoder,
  getU32Encoder,
  lamports,
  type AccountMeta,
  type Address,
  type InstructionPlan,
  type TransactionSigner,
} from "@solana/kit";
import { AeKey, ElGamalKeypair } from "@solana/zk-sdk";
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import {
  decodeSwapDvpChecked,
  findNonceTombstonePda,
  findSwapDvpEscrowAta,
  findSwapDvpPda,
} from "../..";
import {
  EscrowKeys,
  PlannedTransaction,
  applySession,
  confidentialState,
  createSession,
  mintAuditor,
  readAvailableBalance,
  type TransferSource,
  type ConfidentialTransferAccount,
  type ConfidentialAccountKeys,
  type SessionConfig,
  type ExpectedConfidentialTerms,
} from "../../confidential";
import { HOOK_PROGRAM, TestContext, execute, send } from "./context";

const MEMO = address("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");
const INSTRUCTIONS_SYSVAR = address(
  "Sysvar1nstructions1111111111111111111111111",
);
const DECIMALS = 6;
const AMOUNT_A = 75_000n;
const AMOUNT_B = 50_000n;
const EXPIRY_SECONDS = 3600n;

export async function fixture(
  context: TestContext,
  format: 0 | 1,
  options: {
    hook?: boolean;
    amountB?: bigint;
    auditor?: boolean;
    programAddress?: Address;
  } = {},
) {
  const payer = await generateKeyPairSigner();
  const userA = await generateKeyPairSigner();
  const userB = await generateKeyPairSigner();
  const authority = await generateKeyPairSigner();
  const mintA = await generateKeyPairSigner();
  const mintB = await generateKeyPairSigner();
  for (const signer of [payer, userA, userB, authority])
    context.airdrop(signer.address, lamports(10_000_000_000n));
  // Setup uses v1; the scenario's format controls all DvP sessions and funding transfers.
  const setup: SessionConfig = {
    payer,
    format: 1,
    programAddress: options.programAddress,
    minimumBalanceForRentExemption: (space) =>
      context.minimumBalanceForRentExemption(BigInt(space)),
  };
  const keys = EscrowKeys.fromSeed(randomBytes(32));
  const buyerKeys = { elgamal: new ElGamalKeypair(), ae: new AeKey() };
  const recipientKeys = { elgamal: new ElGamalKeypair(), ae: new AeKey() };
  const auditorKey = options.auditor ? new ElGamalKeypair() : undefined;
  const auditorPubkey = auditorKey?.pubkey();
  const extensions: ExtensionArgs[] = [
    {
      __kind: "ConfidentialTransferMint",
      authority: payer.address,
      autoApproveNewAccounts: true,
      auditorElgamalPubkey: auditorPubkey
        ? getAddressDecoder().decode(auditorPubkey.toBytes())
        : null,
    },
  ];
  auditorPubkey?.free();
  auditorKey?.free();
  if (options.hook)
    extensions.push({
      __kind: "TransferHook",
      authority: payer.address,
      programId: HOOK_PROGRAM,
    });

  // Create real mints and configure wallet CT extensions with the Token SDK.
  await createMint(context, setup, mintA, TOKEN_PROGRAM_ADDRESS, []);
  await createMint(
    context,
    setup,
    mintB,
    TOKEN_2022_PROGRAM_ADDRESS,
    extensions,
  );
  if (options.hook) await installHookAccounts(context, mintB.address);
  const refund = await createWallet(
    context,
    setup,
    userB,
    mintB.address,
    buyerKeys,
  );
  const recipient = await createWallet(
    context,
    setup,
    userA,
    mintB.address,
    recipientKeys,
  );
  const assetRefund = await createPublicAta(
    context,
    setup,
    userA.address,
    mintA.address,
  );
  const assetRecipient = await createPublicAta(
    context,
    setup,
    userB.address,
    mintA.address,
  );
  await send(
    context,
    setup,
    new PlannedTransaction([
      getMintToInstruction(
        {
          mint: mintA.address,
          token: assetRefund,
          mintAuthority: payer,
          amount: AMOUNT_A,
        },
        { programAddress: TOKEN_PROGRAM_ADDRESS },
      ),
    ]),
  );

  const seedTerms = {
    settlementAuthority: authority.address,
    userA: userA.address,
    userB: userB.address,
    mintA: mintA.address,
    mintB: mintB.address,
    nonce: 1n,
  };
  const [swapDvp] = await findSwapDvpPda({
    ...seedTerms,
    programAddress: options.programAddress,
  });
  const [nonceTombstone] = await findNonceTombstonePda(
    swapDvp,
    options.programAddress,
  );
  const [dvpAtaA] = await findSwapDvpEscrowAta({
    swapDvp,
    mint: mintA.address,
    tokenProgram: TOKEN_PROGRAM_ADDRESS,
  });
  const [dvpAtaB] = await findSwapDvpEscrowAta({
    swapDvp,
    mint: mintB.address,
    tokenProgram: TOKEN_2022_PROGRAM_ADDRESS,
  });
  const common = {
    swapDvp,
    mintA: mintA.address,
    mintB: mintB.address,
    dvpAtaA,
    dvpAtaB,
    userAAtaA: assetRefund,
    userBAtaB: refund,
    tokenProgramA: TOKEN_PROGRAM_ADDRESS,
    tokenProgramB: TOKEN_2022_PROGRAM_ADDRESS,
    memoProgram: MEMO,
    zkElgamalProofProgram: ZK_ELGAMAL_PROOF_PROGRAM_ADDRESS,
  };
  const config: SessionConfig = {
    ...setup,
    format,
    lookupTables:
      format === 0
        ? await createLookupTable(context, setup, [
            ...Object.values(common),
            nonceTombstone,
            recipient,
            assetRecipient,
            userA.address,
            userB.address,
            authority.address,
            INSTRUCTIONS_SYSVAR,
          ])
        : undefined,
  };
  const create = {
    ...seedTerms,
    payer,
    swapDvp,
    nonceTombstone,
    dvpAtaA,
    dvpAtaB,
    tokenProgramA: TOKEN_PROGRAM_ADDRESS,
    tokenProgramB: TOKEN_2022_PROGRAM_ADDRESS,
    instructionsSysvar: INSTRUCTIONS_SYSVAR,
    amountA: AMOUNT_A,
    expiryTimestamp: context.getClock().unixTimestamp + EXPIRY_SECONDS,
    refString: null,
    userASettlementDestination: null,
    userBSettlementDestination: null,
    earliestSettlementTimestamp: null,
  };
  const apply = {
    ...seedTerms,
    signer: userB,
    swapDvp,
    nonceTombstone,
    dvpAtaB,
    tokenProgram: TOKEN_2022_PROGRAM_ADDRESS,
  };
  const reclaim = {
    signer: userB,
    swapDvp,
    mint: mintB.address,
    dvpSourceAta: dvpAtaB,
    signerDestAta: refund,
    tokenProgram: TOKEN_2022_PROGRAM_ADDRESS,
    memoProgram: MEMO,
    zkElgamalProofProgram: ZK_ELGAMAL_PROOF_PROGRAM_ADDRESS,
  };
  const state = (key: Address) =>
    confidentialState(getTokenDecoder().decode(context.account(key)!.data));
  const source = () => ({ state: state(dvpAtaB), keys });
  const swap = () => {
    const decoded = decodeSwapDvpChecked(
      context.account(swapDvp)!,
      options.programAddress,
    );
    assert(decoded.data.mode === "confidential");
    return decoded.data.confidential;
  };
  return {
    payer,
    userA,
    userB,
    authority,
    config,
    keys,
    buyerKeys,
    recipientKeys,
    common,
    create,
    apply,
    reclaim,
    recipient,
    assetRefund,
    assetRecipient,
    hookProgram: HOOK_PROGRAM,
    seedTerms,
    state,
    source,
    swap,
    amount: options.amountB ?? AMOUNT_B,
    // The agreed public terms, taken from the Create input.
    terms: {
      ...seedTerms,
      tokenProgramA: create.tokenProgramA,
      amountA: create.amountA,
      amountB: options.amountB ?? AMOUNT_B,
      expiryTimestamp: create.expiryTimestamp,
      earliestSettlementTimestamp: null,
      userASettlementDestination: seedTerms.userA,
      userBSettlementDestination: seedTerms.userB,
    } satisfies ExpectedConfidentialTerms,
    auditor: mintAuditor(context.account(mintB.address)!, mintB.address),
    recover: {
      ...seedTerms,
      ...reclaim,
      nonceTombstone,
      dvpEscrowAta: dvpAtaB,
    },
    settle: {
      ...common,
      settlementAuthority: authority,
      userADestinationAtaB: recipient,
      userBDestinationAtaA: assetRecipient,
    },
    close: () => {
      keys.free();
      buyerKeys.elgamal.free();
      buyerKeys.ae.free();
      recipientKeys.elgamal.free();
      recipientKeys.ae.free();
    },
  };
}
export type Fixture = Awaited<ReturnType<typeof fixture>>;

export async function createAndFund(
  context: TestContext,
  f: Fixture,
  amount = f.amount,
  extras: readonly AccountMeta[] = [],
) {
  await execute(
    context,
    f.config,
    createSession(f.config, f.create, f.keys, f.amount),
  );
  await fundAsset(context, f);
  const history = await fundEscrow(context, f, amount, extras);
  history.push(
    ...(await execute(
      context,
      f.config,
      applySession(f.config, f.apply, f.source()),
    )),
  );
  return history;
}

export async function fundAsset(context: TestContext, f: Fixture) {
  return send(
    context,
    f.config,
    new PlannedTransaction([
      getTransferCheckedInstruction(
        {
          source: f.assetRefund,
          mint: f.common.mintA,
          destination: f.common.dvpAtaA,
          authority: f.userA,
          amount: f.create.amountA,
          decimals: DECIMALS,
        },
        { programAddress: TOKEN_PROGRAM_ADDRESS },
      ),
    ]),
  );
}

/** Mint public funds, deposit/apply in the buyer wallet, then transfer to escrow. */
export async function fundEscrow(
  context: TestContext,
  f: Fixture,
  amount: bigint,
  extras: readonly AccountMeta[] = [],
) {
  await send(
    context,
    f.config,
    new PlannedTransaction([
      getMintToInstruction({
        mint: f.common.mintB,
        token: f.common.userBAtaB,
        mintAuthority: f.payer,
        amount,
      }),
      getConfidentialDepositInstruction({
        token: f.common.userBAtaB,
        mint: f.common.mintB,
        authority: f.userB,
        amount,
        decimals: DECIMALS,
      }),
    ]),
  );
  const secret = f.buyerKeys.elgamal.secret();
  try {
    await send(
      context,
      f.config,
      new PlannedTransaction([
        getApplyConfidentialPendingBalanceInstructionFromToken({
          token: f.common.userBAtaB,
          tokenAccount: getTokenDecoder().decode(
            context.account(f.common.userBAtaB)!.data,
          ),
          authority: f.userB,
          elgamalSecretKey: secret,
          aesKey: f.buyerKeys.ae,
        }),
      ]),
    );
  } finally {
    secret.free();
  }
  return execute(
    context,
    f.config,
    await transferSession(
      f.config,
      {
        authority: f.userB,
        source: f.common.userBAtaB,
        mint: f.common.mintB,
        destination: f.common.dvpAtaB,
      },
      {
        source: { state: f.state(f.common.userBAtaB), keys: f.buyerKeys },
        recipient: f.state(f.common.dvpAtaB),
        amount,
        auditor: f.auditor,
      },
      extras,
    ),
  );
}

async function sendSetupPlan(
  context: TestContext,
  config: SessionConfig,
  plan: InstructionPlan,
) {
  const instructions = flattenInstructionPlan(plan).map((entry) => {
    assertIsSingleInstructionPlan(entry);
    return entry.instruction;
  });
  await send(context, config, new PlannedTransaction(instructions));
}

async function createMint(
  context: TestContext,
  config: SessionConfig,
  mint: TransactionSigner,
  tokenProgram: Address,
  extensions: ExtensionArgs[],
) {
  await sendSetupPlan(
    context,
    config,
    await getCreateMintInstructionPlan(
      {
        getMinimumBalance: async (space) =>
          lamports(context.minimumBalanceForRentExemption(BigInt(space))),
      },
      {
        payer: config.payer,
        newMint: mint,
        mintAuthority: config.payer,
        decimals: DECIMALS,
        extensions: extensions.length ? extensions : undefined,
      },
      { tokenProgram },
    ),
  );
}

async function createWallet(
  context: TestContext,
  config: SessionConfig,
  owner: TransactionSigner,
  mint: Address,
  keys: ConfidentialAccountKeys,
) {
  const [token] = await findAssociatedTokenPda({
    owner: owner.address,
    mint,
    tokenProgram: TOKEN_2022_PROGRAM_ADDRESS,
  });
  await sendSetupPlan(
    context,
    config,
    await getCreateConfidentialTransferAccountInstructionPlan({
      rpc: context.rpc,
      payer: config.payer,
      owner,
      mint,
      token,
      elgamalKeypair: keys.elgamal,
      aesKey: keys.ae,
    }),
  );
  return token;
}

async function createPublicAta(
  context: TestContext,
  config: SessionConfig,
  owner: Address,
  mint: Address,
) {
  const ix = await getCreateAssociatedTokenIdempotentInstructionAsync({
    payer: config.payer,
    owner,
    mint,
    tokenProgram: TOKEN_PROGRAM_ADDRESS,
  });
  await send(context, config, new PlannedTransaction([ix]));
  return ix.accounts[1].address;
}

async function createLookupTable(
  context: TestContext,
  config: SessionConfig,
  addresses: Address[],
) {
  const recentSlot = context.getClock().slot;
  const [table] = await findAddressLookupTablePda({
    authority: config.payer.address,
    recentSlot,
  });
  const uniqueAddresses = [...new Set(addresses)];
  await send(
    context,
    config,
    new PlannedTransaction([
      await getCreateLookupTableInstructionAsync({
        authority: config.payer.address,
        payer: config.payer,
        recentSlot,
      }),
      getExtendLookupTableInstruction({
        address: table,
        authority: config.payer,
        payer: config.payer,
        addresses: uniqueAddresses,
      }),
    ]),
  );
  // LUT entries become usable in the slot after extension.
  context.warpToSlot(recentSlot + 1n);
  return { [table]: uniqueAddresses };
}

async function installHookAccounts(context: TestContext, mint: Address) {
  const [key] = await findExtraAccountMetaListPda(
    { mint },
    { programAddress: HOOK_PROGRAM },
  );
  // The no-op test hook has no initializer. Only its empty validation list is injected.
  const data = new Uint8Array(getExtraAccountMetasEncoder().encode([]));
  // The SDK leaves the TLV prefix blank. SPL uses the first eight SHA-256 bytes
  // of the Execute namespace, followed by the encoded list's u32 byte length.
  const discriminator = createHash("sha256")
    .update("spl-transfer-hook-interface:execute")
    .digest()
    .subarray(0, 8);
  const length = getU32Encoder();
  data.set(discriminator);
  data.set(
    length.encode(data.length - discriminator.length - length.fixedSize),
    discriminator.length,
  );
  context.setAccount({
    address: key,
    programAddress: HOOK_PROGRAM,
    data,
    executable: false,
    space: BigInt(data.length),
    lamports: lamports(
      context.minimumBalanceForRentExemption(BigInt(data.length)),
    ),
  });
}

type TransferRequest = Readonly<{
  source: TransferSource;
  recipient: ConfidentialTransferAccount;
  amount: bigint;
  auditor?: Uint8Array;
}>;
async function transferSession(
  config: SessionConfig,
  accounts: {
    authority: TransactionSigner;
    source: Address;
    mint: Address;
    destination: Address;
  },
  request: TransferRequest,
  extras: readonly AccountMeta[] = [],
) {
  const { source } = request;
  const balance = readAvailableBalance(
    source.state,
    source.keys,
    source.history,
  );
  const session = new SessionBuilder(config, accounts.authority);
  const transfer = await prepareTransfer(
    session,
    source.keys,
    new Uint8Array(source.state.availableBalance),
    balance,
    request.amount,
    request.recipient,
    request.auditor,
  );
  const [equalityRecord, ciphertextValidityRecord, rangeRecord] =
    transfer.contexts;
  const ix = getConfidentialTransferInstruction({
    authority: accounts.authority,
    sourceToken: accounts.source,
    mint: accounts.mint,
    destinationToken: accounts.destination,
    equalityRecord,
    ciphertextValidityRecord,
    rangeRecord,
    equalityProofInstructionOffset: 0,
    ciphertextValidityProofInstructionOffset: 0,
    rangeProofInstructionOffset: 0,
    newSourceDecryptableAvailableBalance: new Uint8Array(
      transfer.data.newSourceDecryptableAvailableBalance,
    ),
    transferAmountAuditorCiphertextLo: new Uint8Array(
      transfer.data.auditorCiphertextLo,
    ),
    transferAmountAuditorCiphertextHi: new Uint8Array(
      transfer.data.auditorCiphertextHi,
    ),
  });
  return session.finish([
    { ...ix, accounts: [...ix.accounts, ...hookExtras({ legB: extras })] },
    ...session.closeProofContexts(),
  ]);
}
