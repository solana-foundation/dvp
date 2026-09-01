import { env } from '$env/dynamic/private';
import { json } from '@sveltejs/kit';
import type { RequestHandler } from './$types';
import { allow, clientKey } from '$lib/server/rateLimit';

/**
 * Same-origin JSON-RPC proxy. The browser's Solana RPC client points here; we
 * forward to the keyed devnet endpoint server-side so the RPC key never reaches
 * the client. Retries transient throttling (429 / 5xx) with backoff so a burst
 * of calls during a transaction doesn't surface as a hard error.
 *
 * The endpoint is public, so it forwards only the single, cheap methods the
 * webapp actually calls — no batches, bounded bodies, rate limited per IP and
 * per instance — to keep the keyed provider quota out of reach of abuse.
 */
const ALLOWED_METHODS = new Set([
	'getAccountInfo',
	'getBalance',
	'getLatestBlockhash',
	'getSignatureStatuses',
	'getTokenAccountBalance'
]);
const MAX_BODY_BYTES = 16 * 1024;
const PER_IP = { limit: 300, windowMs: 60_000 };
const GLOBAL = { limit: 1500, windowMs: 60_000 };

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

const rpcError = (id: unknown, code: number, message: string, status: number) =>
	json({ jsonrpc: '2.0', id: id ?? null, error: { code, message } }, { status });

export const POST: RequestHandler = async (event) => {
	if (!env.RPC_URL) {
		return new Response(JSON.stringify({ error: 'RPC_URL not configured' }), {
			status: 500,
			headers: { 'content-type': 'application/json' }
		});
	}
	if (
		!allow(`rpc:${clientKey(event)}`, PER_IP.limit, PER_IP.windowMs) ||
		!allow('rpc:global', GLOBAL.limit, GLOBAL.windowMs)
	) {
		return rpcError(null, -32005, 'Too many requests; slow down', 429);
	}
	const body = await event.request.text();
	if (body.length > MAX_BODY_BYTES) {
		return rpcError(null, -32600, 'Request body too large', 413);
	}
	let call: { id?: unknown; method?: unknown };
	try {
		call = JSON.parse(body);
	} catch {
		return rpcError(null, -32700, 'Parse error', 400);
	}
	if (Array.isArray(call) || typeof call !== 'object' || call === null) {
		return rpcError(null, -32600, 'Batch requests are not supported', 400);
	}
	if (typeof call.method !== 'string' || !ALLOWED_METHODS.has(call.method)) {
		return rpcError(call.id, -32601, 'Method not allowed through this proxy', 403);
	}
	let last: Response | null = null;
	for (let attempt = 0; attempt < 5; attempt++) {
		const upstream = await event.fetch(env.RPC_URL, {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body
		});
		if (upstream.status !== 429 && upstream.status < 500) {
			return new Response(await upstream.text(), {
				status: upstream.status,
				headers: { 'content-type': 'application/json' }
			});
		}
		last = upstream;
		await sleep(250 * 2 ** attempt);
	}
	return new Response(await (last ?? new Response('{}')).text(), {
		status: last?.status ?? 502,
		headers: { 'content-type': 'application/json' }
	});
};
