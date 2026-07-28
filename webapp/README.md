# DvP demo

An interactive walkthrough of the DvP swap program on devnet. One person plays
every actor — Maker, Party A (seller), Party B (buyer), and the Settlement
Authority — and steps through the full lifecycle: create a trade, fund each leg
with a plain token transfer, then settle both legs atomically. Built to make one
idea obvious: **counterparties fund their side with an ordinary `TransferChecked`
to an escrow address, so there is nothing to integrate.**

Program: `dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq` (devnet).

## Stack

- SvelteKit (Svelte 5 runes) + Bun + `@sveltejs/adapter-node`.
- `@solana/kit` + the vendored `dvp-swap-program` client (`src/lib/dvp/`) +
  `@solana-program/token` / `@solana-program/system`.
- App-managed role keypairs live in the browser (localStorage); each signs
  locally, so no wallet extension is needed.
- The role wallets hold **no SOL**: the treasury sponsors every transaction. The
  browser sets the treasury as fee payer and partial-signs the role's part, then
  `/api/relay` adds the treasury signature and submits.
- The keyed RPC and the demo treasury key are **server-only**. The browser talks
  to Solana through the `/api/rpc` same-origin proxy; the faucet (`/api/fund`)
  mints the demo tokens with the treasury key (no SOL is dripped).

## Architecture

| Path | Role |
| ---- | ---- |
| `src/lib/config.ts` | Public config: program id, tokens, preset trade, role list |
| `src/lib/solana/` | Client: RPC proxy, PDAs, role keypairs, the create/fund/settle/… builders |
| `src/lib/server/solana.ts` | Treasury, deterministic demo mints, faucet |
| `src/routes/api/{rpc,config,fund}` | RPC proxy, public config, faucet |
| `src/routes/+page.svelte` | The guided demo console |
| `src/routes/ticket/+page.svelte` | Read-only counterparty view (shareable link) |
| `scripts/smoke.ts` | End-to-end devnet integration test of the whole flow |

## Local development

```sh
bun install
cp .env.example .env      # set RPC_URL and DEMO_TREASURY_SECRET
bun run dev
```

Fund the treasury (the pubkey of `DEMO_TREASURY_SECRET`) with a few devnet SOL
first; it pays the demo's fees/rent and mints the demo tokens. The two demo
mints are created on-chain automatically on the first "Start demo".

Run the integration test (needs `.env`):

```sh
bun scripts/smoke.ts
```

## Deploy (internal artifact platform)

Build must pass at both base paths before deploying:

```sh
bun run build
BASE_PATH=/dvp-demo bun run build
```

`RPC_URL` and `DEMO_TREASURY_SECRET` are secrets — set them via the platform's
encrypted secret upload, never via plain env or the tarball. To let external
counterparties open the shared `/ticket` link, make the app public from the
platform dashboard.
