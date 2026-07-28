/**
 * Server-only Solana helpers. Holds the demo treasury key and the keyed RPC.
 * The treasury is mint authority for the two demo tokens AND the fee payer /
 * rent payer for every user transaction (via `relay`), so the ephemeral role
 * wallets never need any SOL — they only sign to authorize their own action.
 * Nothing here is ever exposed to the browser.
 */
import {
	createKeyPairSignerFromBytes,
	createKeyPairSignerFromPrivateKeyBytes,
	createKeyPairFromBytes,
	address,
	pipe,
	createTransactionMessage,
	setTransactionMessageFeePayerSigner,
	setTransactionMessageLifetimeUsingBlockhash,
	appendTransactionMessageInstructions,
	signTransactionMessageWithSigners,
	signTransaction,
	getBase64EncodedWireTransaction,
	getBase64Encoder,
	getTransactionDecoder,
	getSignatureFromTransaction,
	type Address,
	type KeyPairSigner,
	type Rpc,
	type SolanaRpcApi,
	type Instruction,
	type Signature
} from '@solana/kit';
import {
	getInitializeMint2Instruction,
	getMintSize,
	getMintToInstruction,
	getCreateAssociatedTokenIdempotentInstruction,
	findAssociatedTokenPda,
	TOKEN_PROGRAM_ADDRESS
} from '@solana-program/token';
import { getCreateAccountInstruction } from '@solana-program/system';
import { env } from '$env/dynamic/private';
import { ASSET_TOKEN, CASH_TOKEN } from '$lib/config';
import { resilientRpc } from '$lib/solana/resilientRpc';

const TOKEN = TOKEN_PROGRAM_ADDRESS;
const START_ASSET = 250_00n; // 250 TBILL
const START_CASH = 25_000_000_000n; // 25,000 dUSD

let _secret: Uint8Array | null = null;
let _treasury: Promise<KeyPairSigner> | null = null;
let _treasuryKp: Promise<CryptoKeyPair> | null = null;

function secretBytes(): Uint8Array {
	if (!_secret) {
		if (!env.DEMO_TREASURY_SECRET) throw new Error('DEMO_TREASURY_SECRET is not set');
		_secret = Uint8Array.from(JSON.parse(env.DEMO_TREASURY_SECRET));
	}
	return _secret;
}

function treasury(): Promise<KeyPairSigner> {
	if (!_treasury) _treasury = createKeyPairSignerFromBytes(secretBytes());
	return _treasury;
}

function treasuryKeyPair(): Promise<CryptoKeyPair> {
	if (!_treasuryKp) _treasuryKp = createKeyPairFromBytes(secretBytes());
	return _treasuryKp;
}

export async function treasuryAddress(): Promise<string> {
	return (await treasury()).address;
}

export function serverRpc(): Rpc<SolanaRpcApi> {
	if (!env.RPC_URL) throw new Error('RPC_URL is not set');
	return resilientRpc(env.RPC_URL);
}

async function sha256(data: Uint8Array): Promise<Uint8Array> {
	return new Uint8Array(await crypto.subtle.digest('SHA-256', data));
}

/** Deterministic per-treasury mint keypair, so mint addresses are stable without a DB. */
async function mintSigner(label: string): Promise<KeyPairSigner> {
	const tag = new TextEncoder().encode(':dvp-demo-mint:' + label);
	const seed = await sha256(new Uint8Array([...secretBytes(), ...tag]));
	return createKeyPairSignerFromPrivateKeyBytes(seed);
}

/** Derived mint addresses (no on-chain call). */
export async function mintAddresses(): Promise<{ asset: string; cash: string }> {
	const [asset, cash] = await Promise.all([mintSigner('asset'), mintSigner('cash')]);
	return { asset: asset.address, cash: cash.address };
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function confirm(rpc: Rpc<SolanaRpcApi>, sig: Signature): Promise<void> {
	const start = Date.now();
	while (Date.now() - start < 45_000) {
		const { value } = await rpc.getSignatureStatuses([sig]).send();
		const st = value[0];
		if (st) {
			if (st.err) throw new Error(`Transaction failed: ${JSON.stringify(st.err)}`);
			if (st.confirmationStatus === 'confirmed' || st.confirmationStatus === 'finalized') return;
		}
		await sleep(700);
	}
	throw new Error(`Timed out confirming ${sig}`);
}

/** Build, sign (treasury fee payer), send, confirm — for treasury-only server txs. */
async function send(
	rpc: Rpc<SolanaRpcApi>,
	feePayer: KeyPairSigner,
	ixs: Instruction[]
): Promise<void> {
	const { value: blockhash } = await rpc.getLatestBlockhash({ commitment: 'confirmed' }).send();
	const message = pipe(
		createTransactionMessage({ version: 0 }),
		(m) => setTransactionMessageFeePayerSigner(feePayer, m),
		(m) => setTransactionMessageLifetimeUsingBlockhash(blockhash, m),
		(m) => appendTransactionMessageInstructions(ixs, m)
	);
	const signed = await signTransactionMessageWithSigners(message);
	const sig = getSignatureFromTransaction(signed);
	await rpc
		.sendTransaction(getBase64EncodedWireTransaction(signed), {
			encoding: 'base64',
			preflightCommitment: 'confirmed'
		})
		.send();
	await confirm(rpc, sig);
}

/**
 * Fee sponsorship: take a client-built, role-signed transaction, add the
 * treasury's fee-payer signature, submit and confirm it. The client sets the
 * treasury as fee payer but can't sign for it; this fills that signature.
 */
export async function relay(wireBase64: string): Promise<string> {
	const rpc = serverRpc();
	const bytes = getBase64Encoder().encode(wireBase64);
	const tx = getTransactionDecoder().decode(bytes);
	const signed = await signTransaction([await treasuryKeyPair()], tx);
	const sig = getSignatureFromTransaction(signed);
	await rpc
		.sendTransaction(getBase64EncodedWireTransaction(signed), {
			encoding: 'base64',
			preflightCommitment: 'confirmed'
		})
		.send();
	await confirm(rpc, sig);
	return sig;
}

async function ensureMint(
	rpc: Rpc<SolanaRpcApi>,
	mint: KeyPairSigner,
	decimals: number
): Promise<void> {
	const info = await rpc.getAccountInfo(mint.address, { encoding: 'base64' }).send();
	if (info.value) return;
	const t = await treasury();
	const space = BigInt(getMintSize());
	const lamports = await rpc.getMinimumBalanceForRentExemption(space).send();
	await send(rpc, t, [
		getCreateAccountInstruction({
			payer: t,
			newAccount: mint,
			lamports,
			space,
			programAddress: TOKEN
		}),
		getInitializeMint2Instruction({
			mint: mint.address,
			decimals,
			mintAuthority: t.address,
			freezeAuthority: null
		})
	]);
}

/** Ensure both demo mints exist on-chain; return their addresses. */
export async function ensureMints(
	rpc: Rpc<SolanaRpcApi>
): Promise<{ asset: string; cash: string }> {
	const [asset, cash] = await Promise.all([mintSigner('asset'), mintSigner('cash')]);
	await ensureMint(rpc, asset, ASSET_TOKEN.decimals);
	await ensureMint(rpc, cash, CASH_TOKEN.decimals);
	return { asset: asset.address, cash: cash.address };
}

async function topUpToken(
	rpc: Rpc<SolanaRpcApi>,
	t: KeyPairSigner,
	owner: Address,
	mint: Address,
	target: bigint
): Promise<Instruction[]> {
	const [ata] = await findAssociatedTokenPda({ owner, mint, tokenProgram: TOKEN });
	let bal = 0n;
	try {
		bal = BigInt((await rpc.getTokenAccountBalance(ata).send()).value.amount);
	} catch {
		bal = 0n;
	}
	const ixs: Instruction[] = [
		getCreateAssociatedTokenIdempotentInstruction({ payer: t, owner, mint, ata, tokenProgram: TOKEN })
	];
	if (bal < target) {
		ixs.push(getMintToInstruction({ mint, token: ata, mintAuthority: t, amount: target - bal }));
	}
	return ixs;
}

/**
 * Demo setup: ensure the mints exist and top up Party A's asset and Party B's
 * cash. No SOL is sent — the treasury sponsors all transaction fees via `relay`.
 */
export async function fundRoles(addresses: {
	partyA: string;
	partyB: string;
}): Promise<{ mints: { asset: string; cash: string } }> {
	const rpc = serverRpc();
	const t = await treasury();
	const mints = await ensureMints(rpc);
	const tokenIxs = [
		...(await topUpToken(rpc, t, address(addresses.partyA), address(mints.asset), START_ASSET)),
		...(await topUpToken(rpc, t, address(addresses.partyB), address(mints.cash), START_CASH))
	];
	await send(rpc, t, tokenIxs);
	return { mints };
}
