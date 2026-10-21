import fs from "fs";
import path from "path";
import { preserveConfigFiles } from "./lib/utils";
import { createDvpSwapCodamaBuilder } from "./lib/dvp-swap-codama-builder";
import { patchRustCpiAccountFlags } from "./lib/patch-rust-cpi-account-flags";
import { patchTypeScriptSafeNumbers } from "./lib/patch-typescript-safe-numbers";
import {
  patchRustSwapDvpExactSize,
  patchTypeScriptSwapDvpExactSize,
} from "./lib/patch-swap-dvp-exact-size";
import { renderVisitor as renderRustVisitor } from "@codama/renderers-rust";
import { renderVisitor as renderJavaScriptVisitor } from "@codama/renderers-js";

const projectRoot = path.join(__dirname, "..");
const idlDir = path.join(projectRoot, "idl");
const dvpSwapIdl = JSON.parse(
  fs.readFileSync(path.join(idlDir, "dvp_swap_program.json"), "utf-8"),
);
const rustClientsDir = path.join(__dirname, "..", "clients", "rust");
const typescriptClientsDir = path.join(
  __dirname,
  "..",
  "clients",
  "typescript",
);

const dvpSwapCodama = createDvpSwapCodamaBuilder(dvpSwapIdl)
  .setInstructionAccountDefaultValues()
  .setFixedAccountOptionFields()
  .build();

const configPreserver = preserveConfigFiles(
  typescriptClientsDir,
  rustClientsDir,
);

dvpSwapCodama.accept(
  renderRustVisitor(path.join(rustClientsDir, "src", "generated"), {
    formatCode: true,
    crateFolder: rustClientsDir,
    deleteFolderBeforeRendering: true,
  }),
);

// Correct the swapped CPI remaining-account flags emitted by
// @codama/renderers-rust (see patch-rust-cpi-account-flags.ts).
patchRustCpiAccountFlags(rustClientsDir);

// Reject confidential and other non-458-byte accounts in the generated
// SwapDvp readers (see patch-swap-dvp-exact-size.ts).
patchRustSwapDvpExactSize(rustClientsDir);

// The JS renderer writes asynchronously; await it so the post-render
// patch sees the generated files. Wrapped in an async IIFE because the
// script is transpiled to CJS, which has no top-level await. The explicit
// .catch fails the process with a clean stack trace (e.g. when the
// safety-net patch throws), rather than relying on Node's
// unhandled-rejection behavior.
(async () => {
  await dvpSwapCodama.accept(
    renderJavaScriptVisitor(
      path.join(typescriptClientsDir, "src", "generated"),
      {
        formatCode: true,
        deleteFolderBeforeRendering: true,
      },
    ),
  );

  // Reject unsafe `number` on 64-bit args, which kit's encoders would
  // silently round (see patch-typescript-safe-numbers.ts).
  patchTypeScriptSafeNumbers(typescriptClientsDir);
  patchTypeScriptSwapDvpExactSize(typescriptClientsDir);

  configPreserver.restore();
})().catch((err) => {
  console.error(err);
  process.exit(1);
});
