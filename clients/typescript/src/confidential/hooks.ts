import {
  AccountRole,
  fetchEncodedAccount,
  type AccountMeta,
} from "@solana/kit";
import {
  findExtraAccountMetaListPda,
  resolveExtraAccountMetasForExecute,
  type ResolveExtraAccountMetasForExecuteInput,
} from "@solana-program/token-2022";
import { hookExtras } from "./lifecycle";
import { U64_MAX } from "./constants";

/** Confidential transfers invoke Execute with the hidden amount sentinel. */
export async function resolveConfidentialHookAccounts(
  input: Omit<ResolveExtraAccountMetasForExecuteInput, "amount">,
): Promise<AccountMeta[]> {
  const [validation] = input.validateStatePubkey
    ? [input.validateStatePubkey]
    : await findExtraAccountMetaListPda(
        { mint: input.mint },
        { programAddress: input.transferHookProgramAddress },
      );
  // Validation is optional, but Token-2022 still needs the hook program account.
  // Anyone can fund the PDA, so an empty or foreign-owned account means none.
  const account = await fetchEncodedAccount(input.rpc, validation);
  if (
    !account.exists ||
    account.data.length === 0 ||
    account.programAddress !== input.transferHookProgramAddress
  )
    return [
      { address: input.transferHookProgramAddress, role: AccountRole.READONLY },
    ];
  return hookExtras({
    legB: await resolveExtraAccountMetasForExecute({
      ...input,
      validateStatePubkey: validation,
      amount: U64_MAX,
    }),
  });
}
