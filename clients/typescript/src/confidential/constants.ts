export const BALANCE_BITS = 64n;
export const U64_MAX = (1n << BALANCE_BITS) - 1n;
export const AMOUNT_LO_BITS = 16n;
export const AMOUNT_HI_BITS = 32n;
export const MAX_TRANSFER_AMOUNT =
  (1n << (AMOUNT_LO_BITS + AMOUNT_HI_BITS)) - 1n;
export const ELGAMAL_CIPHERTEXT_SIZE = 64;
