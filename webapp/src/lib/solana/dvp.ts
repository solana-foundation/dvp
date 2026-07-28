import {
	pipe,
	address,
	type Address,
	type Rpc,
	type SolanaRpcApi,
	type TransactionSigner,
	type Instruction,
	createTransactionMessage,
	setTransactionMessageFeePayerSigner,
	setTransactionMessageLifetimeUsingBlockhash,
	appendTransactionMessageInstructions,
	signTransactionMessageWithSigners,
	getBase64EncodedWireTransaction,
	getSignatureFromTransaction
} from '@solana/kit';
import {
	TOKEN_PROGRAM_ADDRESS,
	findAssociatedTokenPda,
	getTransferCheckedInstruction,
	getCreateAssociatedTokenIdempotentInstruction
} from '@solana-program/token';
import {
	getCreateDvpInstruction,
	getSettleDvpInstruction,
	getReclaimDvpInstruction,
	getRejectDvpInstruction,
	getCancelDvpInstruction,
	fetchMaybeSwapDvp
} from '$lib/dvp';
import { PROGRAM_ID } from '$lib/config';
import { deriveDvpAddresses, type DvpAddresses } from './pdas';
import { confirmSignature } from './rpc';

const PROG = address(PROGRAM_ID);
const TOKEN = TOKEN_PROGRAM_ADDRESS;
const MEMO_PROGRAM = address('MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr');

export interface TradeTerms {
	settlementAuthority: Address;
	userA: Address;
	userB: Address;
	mintA: Address;
	mintB: Address;
	amountA: bigint;
	amountB: bigint;
	decimalsA: number;
	decimalsB: number;
	nonce: bigint;
	expiryTimestamp: bigint;
	ref?: string | null;
}

async function ata(owner: Address, mint: Address): Promise<Address> {
	const [a] = await findAssociatedTokenPda({ owner, mint, tokenProgram: TOKEN });
	return a;
}

function createIdempotentAta(
	payer: TransactionSigner,
	owner: Address,
	mint: Address,
	ataAddr: Address
): Instruction {
	return getCreateAssociatedTokenIdempotentInstruction({
		payer,
		owner,
		mint,
		ata: ataAddr,
		tokenProgram: TOKEN
	});
}

/** Assemble, sign (with all referenced signers), send, and confirm. Returns the signature. */
async function sendIxs(
	rpc: Rpc<SolanaRpcApi>,
	feePayer: TransactionSigner,
	ixs: Instruction[]
): Promise<string> {
	const { value: blockhash } = await rpc.getLatestBlockhash({ commitment: 'confirmed' }).send();
	const message = pipe(
		createTransactionMessage({ version: 0 }),
		(m) => setTransactionMessageFeePayerSigner(feePayer, m),
		(m) => setTransactionMessageLifetimeUsingBlockhash(blockhash, m),
		(m) => appendTransactionMessageInstructions(ixs, m)
	);
	const signed = await signTransactionMessageWithSigners(message);
	const signature = getSignatureFromTransaction(signed);
	const wire = getBase64EncodedWireTransaction(signed);
	await rpc.sendTransaction(wire, { encoding: 'base64', preflightCommitment: 'confirmed' }).send();
	await confirmSignature(rpc, signature);
	return signature;
}

/** CreateDvp: allocate the SwapDvp PDA + tombstone + both escrow ATAs. Paid by the maker. */
export async function createDvp(
	rpc: Rpc<SolanaRpcApi>,
	maker: TransactionSigner,
	terms: TradeTerms
): Promise<{ signature: string; addresses: DvpAddresses }> {
	const addresses = await deriveDvpAddresses({ ...terms, tokenProgram: TOKEN });
	const ix = getCreateDvpInstruction(
		{
			payer: maker,
			swapDvp: addresses.swapDvp,
			nonceTombstone: addresses.nonceTombstone,
			settlementAuthority: terms.settlementAuthority,
			userA: terms.userA,
			userB: terms.userB,
			mintA: terms.mintA,
			mintB: terms.mintB,
			dvpAtaA: addresses.escrowA,
			dvpAtaB: addresses.escrowB,
			tokenProgramA: TOKEN,
			tokenProgramB: TOKEN,
			amountA: terms.amountA,
			amountB: terms.amountB,
			expiryTimestamp: terms.expiryTimestamp,
			nonce: terms.nonce,
			refString: terms.ref ?? null,
			userASettlementDestination: null,
			userBSettlementDestination: null,
			earliestSettlementTimestamp: null
		},
		{ programAddress: PROG }
	);
	const signature = await sendIxs(rpc, maker, [ix]);
	return { signature, addresses };
}

/**
 * Fund a leg. This is the crux of the "no integration" story: a plain
 * TransferChecked from the party's token account to the escrow ATA. Any wallet,
 * exchange, or custodian can do this without knowing the DvP program exists.
 */
export async function fundLeg(
	rpc: Rpc<SolanaRpcApi>,
	party: TransactionSigner,
	args: { mint: Address; decimals: number; escrow: Address; amount: bigint }
): Promise<string> {
	const source = await ata(party.address, args.mint);
	const ix = getTransferCheckedInstruction({
		source,
		mint: args.mint,
		destination: args.escrow,
		authority: party,
		amount: args.amount,
		decimals: args.decimals
	});
	return sendIxs(rpc, party, [ix]);
}

/** SettleDvp: atomic cross-transfer of both legs, then close. Paid by the authority. */
export async function settle(
	rpc: Rpc<SolanaRpcApi>,
	authority: TransactionSigner,
	terms: TradeTerms,
	addresses: DvpAddresses
): Promise<string> {
	// Destinations default to the counterparties themselves in this demo.
	const userADestB = await ata(terms.userA, terms.mintB); // cash → seller
	const userBDestA = await ata(terms.userB, terms.mintA); // asset → buyer
	const userAAtaA = await ata(terms.userA, terms.mintA); // asset surplus refund
	const userBAtaB = await ata(terms.userB, terms.mintB); // cash surplus refund

	const ixs: Instruction[] = [
		// All four recipient ATAs must exist before settlement.
		createIdempotentAta(authority, terms.userA, terms.mintB, userADestB),
		createIdempotentAta(authority, terms.userB, terms.mintA, userBDestA),
		createIdempotentAta(authority, terms.userA, terms.mintA, userAAtaA),
		createIdempotentAta(authority, terms.userB, terms.mintB, userBAtaB),
		getSettleDvpInstruction(
			{
				settlementAuthority: authority,
				swapDvp: addresses.swapDvp,
				mintA: terms.mintA,
				mintB: terms.mintB,
				dvpAtaA: addresses.escrowA,
				dvpAtaB: addresses.escrowB,
				userADestinationAtaB: userADestB,
				userBDestinationAtaA: userBDestA,
				userAAtaA,
				userBAtaB,
				tokenProgramA: TOKEN,
				tokenProgramB: TOKEN,
				memoProgram: MEMO_PROGRAM,
				legAExtrasCount: 0
			},
			{ programAddress: PROG }
		)
	];
	return sendIxs(rpc, authority, ixs);
}

/** ReclaimDvp: a party pulls its own leg back while the trade stays open. */
export async function reclaim(
	rpc: Rpc<SolanaRpcApi>,
	party: TransactionSigner,
	args: { swapDvp: Address; mint: Address; escrow: Address }
): Promise<string> {
	const signerDestAta = await ata(party.address, args.mint);
	const ix = getReclaimDvpInstruction(
		{
			signer: party,
			swapDvp: args.swapDvp,
			mint: args.mint,
			dvpSourceAta: args.escrow,
			signerDestAta,
			tokenProgram: TOKEN,
			memoProgram: MEMO_PROGRAM
		},
		{ programAddress: PROG }
	);
	return sendIxs(rpc, party, [ix]);
}

/** RejectDvp (party) / CancelDvp (authority): refund both funded legs and close. */
async function refundAndClose(
	rpc: Rpc<SolanaRpcApi>,
	signer: TransactionSigner,
	terms: TradeTerms,
	addresses: DvpAddresses,
	kind: 'reject' | 'cancel'
): Promise<string> {
	const userAAtaA = await ata(terms.userA, terms.mintA);
	const userBAtaB = await ata(terms.userB, terms.mintB);
	const common = {
		swapDvp: addresses.swapDvp,
		mintA: terms.mintA,
		mintB: terms.mintB,
		dvpAtaA: addresses.escrowA,
		dvpAtaB: addresses.escrowB,
		userAAtaA,
		userBAtaB,
		tokenProgramA: TOKEN,
		tokenProgramB: TOKEN,
		memoProgram: MEMO_PROGRAM,
		legAExtrasCount: 0
	};
	const ix =
		kind === 'reject'
			? getRejectDvpInstruction({ signer, ...common }, { programAddress: PROG })
			: getCancelDvpInstruction({ settlementAuthority: signer, ...common }, { programAddress: PROG });
	return sendIxs(rpc, signer, [ix]);
}

export const reject = (
	rpc: Rpc<SolanaRpcApi>,
	party: TransactionSigner,
	terms: TradeTerms,
	addresses: DvpAddresses
) => refundAndClose(rpc, party, terms, addresses, 'reject');

export const cancel = (
	rpc: Rpc<SolanaRpcApi>,
	authority: TransactionSigner,
	terms: TradeTerms,
	addresses: DvpAddresses
) => refundAndClose(rpc, authority, terms, addresses, 'cancel');

/** Live trade state: whether the SwapDvp still exists and each escrow's balance. */
export interface TradeState {
	open: boolean;
	escrowABalance: bigint;
	escrowBBalance: bigint;
}

async function tokenBalance(rpc: Rpc<SolanaRpcApi>, account: Address): Promise<bigint> {
	try {
		const { value } = await rpc.getTokenAccountBalance(account).send();
		return BigInt(value.amount);
	} catch {
		return 0n;
	}
}

export async function readTradeState(
	rpc: Rpc<SolanaRpcApi>,
	addresses: DvpAddresses
): Promise<TradeState> {
	const [maybe, escrowABalance, escrowBBalance] = await Promise.all([
		fetchMaybeSwapDvp(rpc, addresses.swapDvp),
		tokenBalance(rpc, addresses.escrowA),
		tokenBalance(rpc, addresses.escrowB)
	]);
	return { open: maybe.exists, escrowABalance, escrowBBalance };
}
