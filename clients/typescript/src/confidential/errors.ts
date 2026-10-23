/** Mirrors the Rust client's `ConfidentialError` variants. Branch on `code`, not on the message. */
export type ConfidentialErrorCode =
  | "InvalidAmount"
  | "TransferAmountTooLarge"
  | "ZeroPartialRefund"
  | "InsufficientAvailable"
  | "SurplusTooLarge"
  | "RecipientNotApproved"
  | "RecipientCreditsDisabled"
  | "RecipientPendingCounterFull"
  | "Account"
  | "EscrowKeyMismatch"
  | "AmountMismatch"
  | "BalanceMismatch"
  | "IncompleteHistory"
  | "Arithmetic"
  | "Transaction"
  | "TransactionTooLarge"
  | "HookedSettleRequiresV1";

export class ConfidentialError extends Error {
  constructor(
    readonly code: ConfidentialErrorCode,
    message: string,
    options?: ErrorOptions,
  ) {
    super(message, options);
    this.name = "ConfidentialError";
  }
}
