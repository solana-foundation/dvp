import { env } from '$env/dynamic/private';
import type { RequestHandler } from './$types';

/**
 * Same-origin JSON-RPC proxy. The browser's Solana RPC client points here; we
 * forward to the keyed devnet endpoint server-side so the RPC key never reaches
 * the client. Retries transient throttling (429 / 5xx) with backoff so a burst
 * of calls during a transaction doesn't surface as a hard error.
 */
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export const POST: RequestHandler = async ({ request, fetch }) => {
	if (!env.RPC_URL) {
		return new Response(JSON.stringify({ error: 'RPC_URL not configured' }), {
			status: 500,
			headers: { 'content-type': 'application/json' }
		});
	}
	const body = await request.text();
	let last: Response | null = null;
	for (let attempt = 0; attempt < 5; attempt++) {
		const upstream = await fetch(env.RPC_URL, {
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
