import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { it } from "node:test";
import { findSwapDvpPda, getSwapDvpDecoder } from "../index";

it("exports generated builders and verification without the confidential runtime", () => {
  assert.equal(typeof findSwapDvpPda, "function");
  assert.equal(typeof getSwapDvpDecoder, "function");
  // Each test file runs in its own process. The SDK's Node entrypoint is CommonJS;
  // importing the public client must not load it into the module cache.
  const require = createRequire(resolve("package.json"));
  assert.equal(require.cache[require.resolve("@solana/zk-sdk")], undefined);
});
