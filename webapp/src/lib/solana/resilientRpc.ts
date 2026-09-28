import {
	createDefaultRpcTransport,
	createSolanaRpcFromTransport,
	type Rpc,
	type SolanaRpcApi
} from '@solana/kit';

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

const ATTEMPTS = 6;
// The provider sometimes accepts a connection and then never answers, so bound
// each attempt rather than waiting on a socket that will never produce bytes.
const ATTEMPT_TIMEOUT_MS = 8_000;

/**
 * A Solana RPC whose transport retries transient failures: throttling (429),
 * upstream 5xx, and connection-level faults (refused, DNS, stalls we abort).
 *
 * Everything the transport throws is retried. Matching on error text was too
 * brittle — Bun reports a refused connection as "Unable to connect. Is the
 * computer able to access the url?", which no sensible pattern catches — and
 * the transport only throws on transport faults anyway; a JSON-RPC error comes
 * back as a normal response and is never retried here.
 */
export function resilientRpc(url: string): Rpc<SolanaRpcApi> {
	const inner = createDefaultRpcTransport({ url });
	const transport = (async (config: Parameters<typeof inner>[0]) => {
		let lastErr: unknown;
		for (let attempt = 0; attempt < ATTEMPTS; attempt++) {
			const timeout = AbortSignal.timeout(ATTEMPT_TIMEOUT_MS);
			try {
				return await inner({
					...config,
					signal: config.signal ? AbortSignal.any([config.signal, timeout]) : timeout
				});
			} catch (e) {
				if (config.signal?.aborted) throw e;
				lastErr = e;
				await sleep(250 * 2 ** attempt);
			}
		}
		throw lastErr;
	}) as typeof inner;
	return createSolanaRpcFromTransport(transport);
}
