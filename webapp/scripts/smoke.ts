/**
 * End-to-end devnet smoke test of the DvP flow, using the same vendored client
 * builders the web app uses. Run: `bun scripts/smoke.ts` (reads .env).
 *
 * Exercises: create → fund leg A → fund leg B → settle, and asserts the two
 * legs actually crossed. Uses throwaway keypairs funded by the demo treasury.
 */
import {
	createKeyPairSignerFromBytes,
	generateKeyPairSigner,
	address,
	getProgramDerivedAddress,
	getAddressEncoder,
	pipe,
	createTransactionMessage,
	setTransactionMessageFeePayerSigner,
	setTransactionMessageLifetimeUsingBlockhash,
	appendTransactionMessageInstructions,
	signTransactionMessageWithSigners,
	getBase64EncodedWireTransaction,
	getSignatureFromTransaction,
	type Address,
	type KeyPairSigner,
	type Instruction,
	type Rpc,
	type SolanaRpcApi,
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
import { getCreateAccountInstruction, getTransferSolInstruction } from '@solana-program/system';
import {
	getCreateDvpInstruction,
	getSettleDvpInstruction,
	findSwapDvpPda,
	findSwapDvpEscrowAta
} from '../src/lib/dvp';
import { resilientRpc } from '../src/lib/solana/resilientRpc';

const PROG = address('dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq');
const TOKEN = TOKEN_PROGRAM_ADDRESS;
const MEMO = address('MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr');

const rpc = resilientRpc(process.env.RPC_URL!);
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function confirm(sig: Signature) {
	const start = Date.now();
	while (Date.now() - start < 45_000) {
		const { value } = await rpc.getSignatureStatuses([sig]).send();
		const st = value[0];
		if (st?.err) throw new Error('tx failed: ' + JSON.stringify(st.err));
		if (st && (st.confirmationStatus === 'confirmed' || st.confirmationStatus === 'finalized')) return;
		await sleep(600);
	}
	throw new Error('confirm timeout ' + sig);
}

async function send(feePayer: KeyPairSigner, ixs: Instruction[], label: string) {
	await sleep(1500); // self-pace to stay under the devnet RPC rate limit
	const { value: bh } = await rpc.getLatestBlockhash({ commitment: 'confirmed' }).send();
	const msg = pipe(
		createTransactionMessage({ version: 0 }),
		(m) => setTransactionMessageFeePayerSigner(feePayer, m),
		(m) => setTransactionMessageLifetimeUsingBlockhash(bh, m),
		(m) => appendTransactionMessageInstructions(ixs, m)
	);
	const signed = await signTransactionMessageWithSigners(msg);
	const sig = getSignatureFromTransaction(signed);
	await rpc
		.sendTransaction(getBase64EncodedWireTransaction(signed), {
			encoding: 'base64',
			preflightCommitment: 'confirmed'
		})
		.send();
	await confirm(sig);
	console.log(`  ✓ ${label}: ${sig}`);
	return sig;
}

async function ata(owner: Address, mint: Address): Promise<Address> {
	const [a] = await findAssociatedTokenPda({ owner, mint, tokenProgram: TOKEN });
	return a;
}
async function bal(acc: Address): Promise<bigint> {
	try {
		return BigInt((await rpc.getTokenAccountBalance(acc).send()).value.amount);
	} catch {
		return 0n;
	}
}

async function createMint(treasury: KeyPairSigner, decimals: number): Promise<Address> {
	const mint = await generateKeyPairSigner();
	const space = BigInt(getMintSize());
	const lamports = await rpc.getMinimumBalanceForRentExemption(space).send();
	await send(
		treasury,
		[
			getCreateAccountInstruction({ payer: treasury, newAccount: mint, lamports, space, programAddress: TOKEN }),
			getInitializeMint2Instruction({ mint: mint.address, decimals, mintAuthority: treasury.address, freezeAuthority: null })
		],
		`create mint ${mint.address.slice(0, 6)}`
	);
	return mint.address;
}

async function mintTo(treasury: KeyPairSigner, mint: Address, owner: Address, amount: bigint) {
	const acc = await ata(owner, mint);
	await send(
		treasury,
		[
			getCreateAssociatedTokenIdempotentInstruction({ payer: treasury, owner, mint, ata: acc, tokenProgram: TOKEN }),
			getMintToInstruction({ mint, token: acc, mintAuthority: treasury, amount })
		],
		`mint ${amount} to ${owner.slice(0, 6)}`
	);
}

async function main() {
	const treasury = await createKeyPairSignerFromBytes(Uint8Array.from(JSON.parse(process.env.DEMO_TREASURY_SECRET!)));
	console.log('treasury', treasury.address);

	const [maker, userA, userB, authority] = await Promise.all([
		generateKeyPairSigner(),
		generateKeyPairSigner(),
		generateKeyPairSigner(),
		generateKeyPairSigner()
	]);

	console.log('\n1) fund SOL to actors');
	await send(
		treasury,
		[maker, userA, userB, authority].map((s) =>
			getTransferSolInstruction({ source: treasury, destination: s.address, amount: 60_000_000n })
		),
		'sol drip'
	);

	console.log('\n2) create mints + fund parties');
	const [asset, cash] = await Promise.all([createMint(treasury, 2), createMint(treasury, 6)]);
	await mintTo(treasury, asset, userA.address, 200_00n);
	await mintTo(treasury, cash, userB.address, 20_000_000_000n);

	const amountA = 100_00n;
	const amountB = 10_000_000_000n;
	const nonce = crypto.getRandomValues(new BigUint64Array(1))[0];

	const [swapDvp] = await findSwapDvpPda({
		settlementAuthority: authority.address,
		userA: userA.address,
		userB: userB.address,
		mintA: asset,
		mintB: cash,
		nonce,
		programAddress: PROG
	});
	const [tombstone] = await getProgramDerivedAddress({
		programAddress: PROG,
		seeds: [new TextEncoder().encode('nonce'), getAddressEncoder().encode(swapDvp)]
	});
	const [escrowA] = await findSwapDvpEscrowAta({ swapDvp, mint: asset, tokenProgram: TOKEN });
	const [escrowB] = await findSwapDvpEscrowAta({ swapDvp, mint: cash, tokenProgram: TOKEN });

	console.log('\n3) create DvP', swapDvp);
	await send(
		maker,
		[
			getCreateDvpInstruction(
				{
					payer: maker,
					swapDvp,
					nonceTombstone: tombstone,
					settlementAuthority: authority.address,
					userA: userA.address,
					userB: userB.address,
					mintA: asset,
					mintB: cash,
					dvpAtaA: escrowA,
					dvpAtaB: escrowB,
					tokenProgramA: TOKEN,
					tokenProgramB: TOKEN,
					amountA,
					amountB,
					expiryTimestamp: BigInt(Math.floor(Date.now() / 1000) + 3600),
					nonce,
					refString: 'SMOKE',
					userASettlementDestination: null,
					userBSettlementDestination: null,
					earliestSettlementTimestamp: null
				},
				{ programAddress: PROG }
			)
		],
		'create'
	);

	console.log('\n4) fund both legs (plain TransferChecked to escrow)');
	await send(
		userA,
		[getTransferCheckedInstruction({ source: await ata(userA.address, asset), mint: asset, destination: escrowA, authority: userA, amount: amountA, decimals: 2 })],
		'fund asset leg'
	);
	await send(
		userB,
		[getTransferCheckedInstruction({ source: await ata(userB.address, cash), mint: cash, destination: escrowB, authority: userB, amount: amountB, decimals: 6 })],
		'fund cash leg'
	);
	console.log('  escrowA', await bal(escrowA), 'escrowB', await bal(escrowB));

	console.log('\n5) settle atomically');
	const uaDestB = await ata(userA.address, cash);
	const ubDestA = await ata(userB.address, asset);
	const uaA = await ata(userA.address, asset);
	const ubB = await ata(userB.address, cash);
	await send(
		authority,
		[
			getCreateAssociatedTokenIdempotentInstruction({ payer: authority, owner: userA.address, mint: cash, ata: uaDestB, tokenProgram: TOKEN }),
			getCreateAssociatedTokenIdempotentInstruction({ payer: authority, owner: userB.address, mint: asset, ata: ubDestA, tokenProgram: TOKEN }),
			getCreateAssociatedTokenIdempotentInstruction({ payer: authority, owner: userA.address, mint: asset, ata: uaA, tokenProgram: TOKEN }),
			getCreateAssociatedTokenIdempotentInstruction({ payer: authority, owner: userB.address, mint: cash, ata: ubB, tokenProgram: TOKEN }),
			getSettleDvpInstruction(
				{
					settlementAuthority: authority,
					swapDvp,
					mintA: asset,
					mintB: cash,
					dvpAtaA: escrowA,
					dvpAtaB: escrowB,
					userADestinationAtaB: uaDestB,
					userBDestinationAtaA: ubDestA,
					userAAtaA: uaA,
					userBAtaB: ubB,
					tokenProgramA: TOKEN,
					tokenProgramB: TOKEN,
					memoProgram: MEMO,
					legAExtrasCount: 0
				},
				{ programAddress: PROG }
			)
		],
		'settle'
	);

	const aCash = await bal(uaDestB);
	const bAsset = await bal(ubDestA);
	console.log('\nRESULT: Party A received', aCash, 'dUSD; Party B received', bAsset, 'TBILL');
	if (aCash === amountB && bAsset === amountA) console.log('\n✅ SMOKE PASSED: legs crossed atomically');
	else throw new Error('❌ balances did not match expected swap');
}

main().catch((e) => {
	console.error('\n❌ SMOKE FAILED:', e.message);
	process.exit(1);
});
