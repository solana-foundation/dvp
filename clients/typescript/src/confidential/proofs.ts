import { getAddressEncoder } from "@solana/kit";
import {
  BatchedGroupedCiphertext3HandlesValidityProofData,
  BatchedRangeProofU128Data,
  CiphertextCommitmentEqualityProofData,
  ElGamalPubkey,
  GroupedElGamalCiphertext3Handles,
  PedersenCommitment,
  PedersenOpening,
} from "@solana/zk-sdk";
import { type ConfidentialAccountKeys } from "./keys";
import {
  BALANCE_BITS,
  AMOUNT_LO_BITS,
  AMOUNT_HI_BITS,
  MAX_TRANSFER_AMOUNT,
} from "./constants";
import {
  parseCiphertext,
  extractCiphertext,
  subtractTransfer,
  assertU64,
} from "./math";
import type { ConfidentialTransferAccount } from "./balance";
import { ProofKind, SessionBuilder } from "./transaction";
import { ConfidentialError } from "./errors";

// Complete the U128 range proof after the balance and two transfer limbs.
const RANGE_PROOF_PADDING_BITS = 16n;

export function checkRecipient(
  state: ConfidentialTransferAccount,
  requiredCredits = 1n,
): void {
  if (!state.approved)
    throw new ConfidentialError(
      "RecipientNotApproved",
      "Recipient is not approved",
    );
  if (!state.allowConfidentialCredits)
    throw new ConfidentialError(
      "RecipientCreditsDisabled",
      "Recipient confidential credits disabled",
    );
  const pendingCredits = state.pendingBalanceCreditCounter + requiredCredits;
  assertU64(pendingCredits);
  if (pendingCredits > state.maximumPendingBalanceCreditCounter)
    throw new ConfidentialError(
      "RecipientPendingCounterFull",
      "Recipient pending counter is full",
    );
}

/** Generate the same split transfer proof as the Token-2022 Rust client. */
export async function prepareTransfer(
  session: SessionBuilder,
  keys: ConfidentialAccountKeys,
  available: Uint8Array,
  balance: bigint,
  amount: bigint,
  recipient: ConfidentialTransferAccount,
  auditor?: Uint8Array,
) {
  if (amount < 0n || amount > MAX_TRANSFER_AMOUNT)
    throw new ConfidentialError(
      "TransferAmountTooLarge",
      "Transfer amount must be in 0..2^48-1",
    );
  if (amount > balance)
    throw new ConfidentialError(
      "InsufficientAvailable",
      "Insufficient available balance",
    );
  checkRecipient(recipient);
  // Free all temporary WASM objects even if proof construction or planning fails.
  const allocations = new Set<{ free(): void }>();
  const keep = <T extends { free(): void }>(value: T): T => {
    allocations.add(value);
    return value;
  };
  // wasm-bindgen moves Vec<T> arguments into Rust; only borrowed objects remain ours to free.
  const move = <T extends { free(): void }>(value: T): T => {
    allocations.delete(value);
    return value;
  };
  try {
    const source = keep(keys.elgamal.pubkey());
    const destination = keep(
      ElGamalPubkey.fromBytes(
        new Uint8Array(getAddressEncoder().encode(recipient.elgamalPubkey)),
      ),
    );
    const auditorKey = keep(
      ElGamalPubkey.fromBytes(auditor ?? new Uint8Array(32)),
    );
    const loAmount = amount & ((1n << AMOUNT_LO_BITS) - 1n),
      hiAmount = amount >> AMOUNT_LO_BITS;
    const loOpening = keep(new PedersenOpening()),
      hiOpening = keep(new PedersenOpening());
    const lo = keep(
      GroupedElGamalCiphertext3Handles.encryptWith(
        source,
        destination,
        auditorKey,
        loAmount,
        loOpening,
      ),
    );
    const hi = keep(
      GroupedElGamalCiphertext3Handles.encryptWith(
        source,
        destination,
        auditorKey,
        hiAmount,
        hiOpening,
      ),
    );
    const limbs = [
      extractCiphertext(lo.toBytes(), 0),
      extractCiphertext(hi.toBytes(), 0),
    ] as const;
    const remaining = subtractTransfer(available, ...limbs);
    const remainingCiphertext = keep(parseCiphertext(remaining));
    const remainingOpening = keep(new PedersenOpening());
    const remainingCommitment = keep(
      PedersenCommitment.from(balance - amount, remainingOpening),
    );
    const equality = keep(
      new CiphertextCommitmentEqualityProofData(
        keys.elgamal,
        remainingCiphertext,
        remainingCommitment,
        remainingOpening,
        balance - amount,
      ),
    );
    const validity = keep(
      new BatchedGroupedCiphertext3HandlesValidityProofData(
        source,
        destination,
        auditorKey,
        lo,
        hi,
        loAmount,
        hiAmount,
        loOpening,
        hiOpening,
      ),
    );
    const paddingOpening = keep(new PedersenOpening());
    const padding = keep(PedersenCommitment.from(0n, paddingOpening));
    const loCommitment = keep(
      PedersenCommitment.fromBytes(lo.toBytes().slice(0, 32)),
    );
    const hiCommitment = keep(
      PedersenCommitment.fromBytes(hi.toBytes().slice(0, 32)),
    );
    // The bit widths follow the commitment order and sum to U128's 128 bits.
    const range = keep(
      new BatchedRangeProofU128Data(
        [remainingCommitment, loCommitment, hiCommitment, padding].map(move),
        new BigUint64Array([balance - amount, loAmount, hiAmount, 0n]),
        new Uint8Array(
          [
            BALANCE_BITS,
            AMOUNT_LO_BITS,
            AMOUNT_HI_BITS,
            RANGE_PROOF_PADDING_BITS,
          ].map(Number),
        ),
        [remainingOpening, loOpening, hiOpening, paddingOpening].map(move),
      ),
    );
    const contexts = [
      await session.proof(ProofKind.CommitmentEquality, equality),
      await session.proof(ProofKind.GroupedValidity, validity),
      await session.proof(ProofKind.Range128, range),
    ] as const;
    const ae = keep(keys.ae.encrypt(balance - amount));
    return {
      contexts,
      remaining,
      limbs,
      data: {
        newSourceDecryptableAvailableBalance: Array.from(ae.toBytes()),
        auditorCiphertextLo: Array.from(extractCiphertext(lo.toBytes(), 2)),
        auditorCiphertextHi: Array.from(extractCiphertext(hi.toBytes(), 2)),
      },
    };
  } finally {
    for (const value of [...allocations].reverse()) value.free();
  }
}
