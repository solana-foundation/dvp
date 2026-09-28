import {
	createDefaultRpcTransport,
	createSolanaRpcFromTransport,
	type Rpc,
	type RpcTransport,
	type SolanaRpcApi,
	type Signature
} from '@solana/kit';
import { base } from '$app/paths';

/** The proxy spends at most 10s upstream, so give it a little room and no more. */
const REQUEST_TIMEOUT_MS = 15_000;

/**
 * Browser RPC. Every call is POSTed to our same-origin /api/rpc proxy, which
 * forwards to the keyed devnet endpoint server-side (the RPC key never reaches
 * the client). Confirmation is by polling — no websocket subscription needed.
 *
 * Every request carries a timeout: the proxy retries upstream itself, so a call
 * that outlives this is wedged, and failing it keeps a stalled read from holding
 * the UI's busy flag open.
 */
export function makeRpc(): Rpc<SolanaRpcApi> {
	const origin = typeof location !== 'undefined' ? location.origin : 'http://localhost';
	const inner = createDefaultRpcTransport({
		url: new URL(`${base}/api/rpc`, origin).href
	});
	const transport = ((config) => {
		const timeout = AbortSignal.timeout(REQUEST_TIMEOUT_MS);
		return inner({
			...config,
			signal: config.signal ? AbortSignal.any([config.signal, timeout]) : timeout
		});
	}) as RpcTransport;
	return createSolanaRpcFromTransport(transport);
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Poll getSignatureStatuses until confirmed/finalized or the tx errors. */
export async function confirmSignature(
	rpc: Rpc<SolanaRpcApi>,
	sig: Signature,
	{ timeoutMs = 45_000 }: { timeoutMs?: number } = {}
): Promise<void> {
	const start = Date.now();
	while (Date.now() - start < timeoutMs) {
		const { value } = await rpc
			.getSignatureStatuses([sig], { searchTransactionHistory: false })
			.send();
		const st = value[0];
		if (st) {
			if (st.err) throw new Error(`Transaction failed: ${JSON.stringify(st.err)}`);
			if (st.confirmationStatus === 'confirmed' || st.confirmationStatus === 'finalized') return;
		}
		await sleep(700);
	}
	throw new Error(`Timed out confirming ${sig}`);
}
