/** Create one open DvP (no funding) and print its address, to eyeball /ticket. */
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
	type KeyPairSigner,
	type Instruction
} from '@solana/kit';
import { TOKEN_PROGRAM_ADDRESS } from '@solana-program/token';
import { getTransferSolInstruction } from '@solana-program/system';
import { getCreateDvpInstruction, findSwapDvpPda, findSwapDvpEscrowAta } from '../src/lib/dvp';
import { resilientRpc } from '../src/lib/solana/resilientRpc';

const PROG = address('dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq');
const TOKEN = TOKEN_PROGRAM_ADDRESS;
const asset = address('GWKGB6HgdUMefNmXnb6RCW834d7QfPyCzA6xhCTqc5jh');
const cash = address('2vhfm5xVSPi4wkGRn6G3qSHdP3dL8KmRsztoMi6kcYYU');
const rpc = resilientRpc(process.env.RPC_URL!);
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function send(feePayer: KeyPairSigner, ixs: Instruction[]) {
	await sleep(1200);
	const { value: bh } = await rpc.getLatestBlockhash({ commitment: 'confirmed' }).send();
	const msg = pipe(
		createTransactionMessage({ version: 0 }),
		(m) => setTransactionMessageFeePayerSigner(feePayer, m),
		(m) => setTransactionMessageLifetimeUsingBlockhash(bh, m),
		(m) => appendTransactionMessageInstructions(ixs, m)
	);
	const signed = await signTransactionMessageWithSigners(msg);
	const sig = getSignatureFromTransaction(signed);
	await rpc.sendTransaction(getBase64EncodedWireTransaction(signed), { encoding: 'base64', preflightCommitment: 'confirmed' }).send();
	for (let i = 0; i < 40; i++) {
		const st = (await rpc.getSignatureStatuses([sig]).send()).value[0];
		if (st?.err) throw new Error(JSON.stringify(st.err));
		if (st?.confirmationStatus === 'confirmed' || st?.confirmationStatus === 'finalized') return;
		await sleep(600);
	}
}

const treasury = await createKeyPairSignerFromBytes(Uint8Array.from(JSON.parse(process.env.DEMO_TREASURY_SECRET!)));
const [maker, userA, userB, authority] = await Promise.all([
	generateKeyPairSigner(),
	generateKeyPairSigner(),
	generateKeyPairSigner(),
	generateKeyPairSigner()
]);
await send(treasury, [getTransferSolInstruction({ source: treasury, destination: maker.address, amount: 40_000_000n })]);

const nonce = crypto.getRandomValues(new BigUint64Array(1))[0];
const [swapDvp] = await findSwapDvpPda({ settlementAuthority: authority.address, userA: userA.address, userB: userB.address, mintA: asset, mintB: cash, nonce, programAddress: PROG });
const [tombstone] = await getProgramDerivedAddress({ programAddress: PROG, seeds: [new TextEncoder().encode('nonce'), getAddressEncoder().encode(swapDvp)] });
const [escrowA] = await findSwapDvpEscrowAta({ swapDvp, mint: asset, tokenProgram: TOKEN });
const [escrowB] = await findSwapDvpEscrowAta({ swapDvp, mint: cash, tokenProgram: TOKEN });

await send(maker, [
	getCreateDvpInstruction(
		{
			payer: maker, swapDvp, nonceTombstone: tombstone, settlementAuthority: authority.address,
			userA: userA.address, userB: userB.address, mintA: asset, mintB: cash,
			dvpAtaA: escrowA, dvpAtaB: escrowB, tokenProgramA: TOKEN, tokenProgramB: TOKEN,
			amountA: 100_00n, amountB: 10_000_000_000n,
			expiryTimestamp: BigInt(Math.floor(Date.now() / 1000) + 3600), nonce,
			refString: 'TICKET', userASettlementDestination: null, userBSettlementDestination: null, earliestSettlementTimestamp: null
		},
		{ programAddress: PROG }
	)
]);
console.log('OPEN_DVP', swapDvp);
