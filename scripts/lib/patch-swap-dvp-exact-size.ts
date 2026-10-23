import fs from "fs";
import path from "path";

/**
 * Makes the generated `SwapDvp` readers reject anything but the 458-byte
 * public layout.
 *
 * The rendered Borsh (Rust) and kit (TypeScript) decoders ignore trailing
 * bytes, so they read the base of a 586-byte confidential account as an
 * ordinary public `SwapDvp` and drop its amount B ciphertexts. Every generated
 * reader goes through one decode function per language:
 *   - Rust: `SwapDvp::from_bytes`, which `fetch_*` already call; the
 *     `TryFrom<&AccountInfo>` impl is rewired to it.
 *   - TypeScript: `decodeSwapDvp`, which backs all four fetch helpers.
 * Those functions now fail on any other size. Owner and PDA are still
 * unchecked; the checked helpers in `verify` remain the funding gate.
 *
 * A safety net asserts the size check is present, so a codama template change
 * that makes the patch no-op fails codegen instead of shipping the old reader.
 */
const RUST_MARKER = "data.len() != crate::verify::SWAP_DVP_ACCOUNT_LEN";
const TYPESCRIPT_MARKER = "getSwapDvpDecoder().fixedSize";

export function patchRustSwapDvpExactSize(rustClientsDir: string): void {
  const filePath = path.join(
    rustClientsDir,
    "src",
    "generated",
    "accounts",
    "swap_dvp.rs",
  );
  const src = fs.readFileSync(filePath, "utf-8");
  const fromBytes = `    pub fn from_bytes(data: &[u8]) -> Result<Self, std::io::Error> {
        let mut data = data;`;
  const tryFrom = `        let mut data: &[u8] = &(*account_info.data).borrow();
        Self::deserialize(&mut data)`;
  const patched = src
    .replace(
      fromBytes,
      `    pub fn from_bytes(data: &[u8]) -> Result<Self, std::io::Error> {
        // Patched by scripts/lib/patch-swap-dvp-exact-size.ts.
        if ${RUST_MARKER} {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "SwapDvp account must be {} bytes, got {}",
                    crate::verify::SWAP_DVP_ACCOUNT_LEN,
                    data.len()
                ),
            ));
        }
        let mut data = data;`,
    )
    .replace(tryFrom, `        Self::from_bytes(&account_info.data.borrow())`);
  fs.writeFileSync(filePath, patched);

  if (!patched.includes(RUST_MARKER) || patched.includes(tryFrom)) {
    throw new Error(
      `patchRustSwapDvpExactSize: size check missing in ${filePath}`,
    );
  }
  console.log("Pinned the generated Rust SwapDvp readers to 458 bytes.");
}

export function patchTypeScriptSwapDvpExactSize(
  typescriptClientsDir: string,
): void {
  const filePath = path.join(
    typescriptClientsDir,
    "src",
    "generated",
    "accounts",
    "swapDvp.ts",
  );
  const src = fs.readFileSync(filePath, "utf-8");
  const decode = `): Account<SwapDvp, TAddress> | MaybeAccount<SwapDvp, TAddress> {
  return decodeAccount(`;
  const patched = src.replace(
    decode,
    `): Account<SwapDvp, TAddress> | MaybeAccount<SwapDvp, TAddress> {
  // Patched by scripts/lib/patch-swap-dvp-exact-size.ts.
  if (
    (!("exists" in encodedAccount) || encodedAccount.exists) &&
    encodedAccount.data.length !== ${TYPESCRIPT_MARKER}
  )
    throw new Error(
      \`SwapDvp account must be \${getSwapDvpDecoder().fixedSize} bytes, got \${encodedAccount.data.length}\`,
    );
  return decodeAccount(`,
  );
  fs.writeFileSync(filePath, patched);

  if (!patched.includes(TYPESCRIPT_MARKER)) {
    throw new Error(
      `patchTypeScriptSwapDvpExactSize: size check missing in ${filePath}`,
    );
  }
  console.log("Pinned the generated TypeScript SwapDvp decoder to 458 bytes.");
}
