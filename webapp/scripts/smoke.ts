/**
 * End-to-end devnet smoke test of the fee-sponsored DvP flow. Role wallets hold
 * ZERO SOL: they only partial-sign, and the treasury co-signs as fee payer
 * (mirroring the browser + /api/relay split). Run: `bun scripts/smoke.ts`.
 */
import {
	createKeyPairSignerFromBytes,
	createKeyPairFromBytes,
	generateKeyPairSigner,
	address,
	createNoopSigner,
	pipe,
	createTransactionMessage,
	setTransactionMessageFeePayer,
	setTransactionMessageFeePayerSigner,
	setTransactionMessageLifetimeUsingBlockhash,
	appendTransactionMessageInstructions,
	partiallySignTransactionMessageWithSigners,
	signTransactionMessageWithSigners,
	signTransaction,
	getTransactionEncoder,
	getTransactionDecoder,
	getBase64Decoder,
	getBase64Encoder,
	getBase64EncodedWireTransaction,
	getSignatureFromTransaction,
	getProgramDerivedAddress,
	getAddressEncoder,
	type Address,
	type KeyPairSigner,
	type Instruction,
	type Signature
} from '@solana/kit';
import {
	TOKEN_PROGRAM_ADDRESS,
	getMintSize,
	getInitializeMint2Instruction,
	getMintToInstruction,
	getCreateAssociatedTokenIdempotentInstruction,
	findAssociatedTokenPda,
	getTransferCheckedInstruction
} from '@solana-program/token';
import { getCreateAccountInstruction } from '@solana-program/system';
import { getCreateDvpInstruction, getSettleDvpInstruction, findSwapDvpPda, findSwapDvpEscrowAta } from '../src/lib/dvp';
import { resilientRpc } from '../src/lib/solana/resilientRpc';

const PROG = address('dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq');
const TOKEN = TOKEN_PROGRAM_ADDRESS;
const MEMO = address('MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr');
const rpc = resilientRpc(process.env.RPC_URL!);
const secret = Uint8Array.from(JSON.parse(process.env.DEMO_TREASURY_SECRET!));
const treasury = await createKeyPairSignerFromBytes(secret);
const treasuryKp = await createKeyPairFromBytes(secret);
const treasuryNoop = createNoopSigner(treasury.address); // one instance, reused per tx
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function confirm(sig: Signature) {
	const start = Date.now();
	while (Date.now() - start < 45_000) {
		const st = (await rpc.getSignatureStatuses([sig]).send()).value[0];
		if (st?.err) throw new Error('tx failed: ' + JSON.stringify(st.err));
		if (st?.confirmationStatus === 'confirmed' || st?.confirmationStatus === 'finalized') return;
		await sleep(600);
	}
	throw new Error('timeout ' + sig);
}

/** Treasury-only tx (mint setup): treasury is fee payer + signer directly. */
async function sendTreasury(ixs: Instruction[], label: string) {
	await sleep(1000);
	const { value: bh } = await rpc.getLatestBlockhash({ commitment: 'confirmed' }).send();
	const msg = pipe(
		createTransactionMessage({ version: 0 }),
		(m) => setTransactionMessageFeePayerSigner(treasury, m),
		(m) => setTransactionMessageLifetimeUsingBlockhash(bh, m),
		(m) => appendTransactionMessageInstructions(ixs, m)
	);
	const signed = await signTransactionMessageWithSigners(msg);
	const sig = getSignatureFromTransaction(signed);
	await rpc.sendTransaction(getBase64EncodedWireTransaction(signed), { encoding: 'base64', preflightCommitment: 'confirmed' }).send();
	await confirm(sig);
	console.log('  ✓', label, sig);
}

/** Sponsored tx: build with treasury as fee-payer ADDRESS, partial-sign roles ("client"),
 *  then decode + treasury co-sign + submit ("server relay"). */
async function sendSponsored(ixs: Instruction[], label: string) {
	await sleep(1200);
	const { value: bh } = await rpc.getLatestBlockhash({ commitment: 'confirmed' }).send();
	const msg = pipe(
		createTransactionMessage({ version: 0 }),
		(m) => setTransactionMessageFeePayer(treasury.address, m),
		(m) => setTransactionMessageLifetimeUsingBlockhash(bh, m),
		(m) => appendTransactionMessageInstructions(ixs, m)
	);
	const partial = await partiallySignTransactionMessageWithSigners(msg);
	const wire = getBase64Decoder().decode(getTransactionEncoder().encode(partial)); // client → server
	const tx = getTransactionDecoder().decode(getBase64Encoder().encode(wire)); // server relay
	const signed = await signTransaction([treasuryKp], tx);
	const sig = getSignatureFromTransaction(signed);
	await rpc.sendTransaction(getBase64EncodedWireTransaction(signed), { encoding: 'base64', preflightCommitment: 'confirmed' }).send();
	await confirm(sig);
	console.log('  ✓', label, sig);
}

const ata = async (owner: Address, mint: Address) => (await findAssociatedTokenPda({ owner, mint, tokenProgram: TOKEN }))[0];
const bal = async (a: Address) => {
	try {
		return BigInt((await rpc.getTokenAccountBalance(a).send()).value.amount);
	} catch {
		return 0n;
	}
};

async function createMint(decimals: number): Promise<Address> {
	const mint = await generateKeyPairSigner();
	const space = BigInt(getMintSize());
	const lamports = await rpc.getMinimumBalanceForRentExemption(space).send();
	await sendTreasury(
		[
			getCreateAccountInstruction({ payer: treasury, newAccount: mint, lamports, space, programAddress: TOKEN }),
			getInitializeMint2Instruction({ mint: mint.address, decimals, mintAuthority: treasury.address, freezeAuthority: null })
		],
		`mint ${mint.address.slice(0, 6)}`
	);
	return mint.address;
}
async function mintTo(mint: Address, owner: Address, amount: bigint) {
	const acc = await ata(owner, mint);
	await sendTreasury(
		[
			getCreateAssociatedTokenIdempotentInstruction({ payer: treasury, owner, mint, ata: acc, tokenProgram: TOKEN }),
			getMintToInstruction({ mint, token: acc, mintAuthority: treasury, amount })
		],
		`fund ${owner.slice(0, 6)}`
	);
}

console.log('treasury', treasury.address);
const [userA, userB, authority] = await Promise.all([generateKeyPairSigner(), generateKeyPairSigner(), generateKeyPairSigner()]);
console.log('roles (all 0 SOL):', userA.address.slice(0, 6), userB.address.slice(0, 6), authority.address.slice(0, 6));

console.log('\n1) mints + token funding (treasury)');
const [asset, cash] = [await createMint(2), await createMint(6)];
await mintTo(asset, userA.address, 200_00n);
await mintTo(cash, userB.address, 20_000_000_000n);

const amountA = 100_00n, amountB = 10_000_000_000n;
const nonce = crypto.getRandomValues(new BigUint64Array(1))[0];
const [swapDvp] = await findSwapDvpPda({ settlementAuthority: authority.address, userA: userA.address, userB: userB.address, mintA: asset, mintB: cash, nonce, programAddress: PROG });
const [tombstone] = await getProgramDerivedAddress({ programAddress: PROG, seeds: [new TextEncoder().encode('nonce'), getAddressEncoder().encode(swapDvp)] });
const [escrowA] = await findSwapDvpEscrowAta({ swapDvp, mint: asset, tokenProgram: TOKEN });
const [escrowB] = await findSwapDvpEscrowAta({ swapDvp, mint: cash, tokenProgram: TOKEN });

console.log('\n2) create (sponsored, treasury pays rent)', swapDvp);
await sendSponsored(
	[
		getCreateDvpInstruction(
			{
				payer: treasuryNoop, swapDvp, nonceTombstone: tombstone, settlementAuthority: authority.address,
				userA: userA.address, userB: userB.address, mintA: asset, mintB: cash, dvpAtaA: escrowA, dvpAtaB: escrowB,
				tokenProgramA: TOKEN, tokenProgramB: TOKEN, amountA, amountB,
				expiryTimestamp: BigInt(Math.floor(Date.now() / 1000) + 3600), nonce,
				refString: 'SPONSORED', userASettlementDestination: null, userBSettlementDestination: null, earliestSettlementTimestamp: null
			},
			{ programAddress: PROG }
		)
	],
	'create'
);

console.log('\n3) fund both legs (party signs, treasury pays fee)');
await sendSponsored([getTransferCheckedInstruction({ source: await ata(userA.address, asset), mint: asset, destination: escrowA, authority: userA, amount: amountA, decimals: 2 })], 'fund asset');
await sendSponsored([getTransferCheckedInstruction({ source: await ata(userB.address, cash), mint: cash, destination: escrowB, authority: userB, amount: amountB, decimals: 6 })], 'fund cash');

console.log('\n4) settle (authority signs, treasury pays fee + ATA rent)');
const uaDestB = await ata(userA.address, cash), ubDestA = await ata(userB.address, asset), uaA = await ata(userA.address, asset), ubB = await ata(userB.address, cash);
const mk = (o: Address, m: Address, a: Address) => getCreateAssociatedTokenIdempotentInstruction({ payer: treasuryNoop, owner: o, mint: m, ata: a, tokenProgram: TOKEN });
await sendSponsored(
	[
		mk(userA.address, cash, uaDestB), mk(userB.address, asset, ubDestA), mk(userA.address, asset, uaA), mk(userB.address, cash, ubB),
		getSettleDvpInstruction(
			{ settlementAuthority: authority, swapDvp, mintA: asset, mintB: cash, dvpAtaA: escrowA, dvpAtaB: escrowB, userADestinationAtaB: uaDestB, userBDestinationAtaA: ubDestA, userAAtaA: uaA, userBAtaB: ubB, tokenProgramA: TOKEN, tokenProgramB: TOKEN, memoProgram: MEMO, legAExtrasCount: 0 },
			{ programAddress: PROG }
		)
	],
	'settle'
);

const aCash = await bal(uaDestB), bAsset = await bal(ubDestA);
console.log('\nRESULT: A received', aCash, 'dUSD; B received', bAsset, 'TBILL');

console.log('\n5) account cleanup after settle');
const info = async (a: Address) => (await rpc.getAccountInfo(a, { encoding: 'base64' }).send()).value;
const swapInfo = await info(swapDvp), eaInfo = await info(escrowA), ebInfo = await info(escrowB), tombInfo = await info(tombstone);
console.log('  swapDvp     :', swapInfo ? `OPEN (${swapInfo.lamports} lamports)` : 'closed ✓');
console.log('  escrow A    :', eaInfo ? 'OPEN' : 'closed ✓');
console.log('  escrow B    :', ebInfo ? 'OPEN' : 'closed ✓');
console.log('  tombstone   :', tombInfo ? `kept (owner ${tombInfo.owner.slice(0, 4)}…, permanent by design)` : 'MISSING');
console.log('  A cash ATA  :', await bal(uaDestB), 'dUSD (kept: holds received cash)');
console.log('  B asset ATA :', await bal(ubDestA), 'TBILL (kept: holds received asset)');

const cleaned = !swapInfo && !eaInfo && !ebInfo;
if (aCash === amountB && bAsset === amountA && cleaned && tombInfo)
	console.log('\n✅ PASSED: legs crossed, roles paid 0 SOL, DvP + both escrows closed, tombstone retained');
else throw new Error('❌ verification failed (balances or cleanup)');
