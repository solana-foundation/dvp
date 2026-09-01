import { json, error } from '@sveltejs/kit';
import type { RequestHandler } from './$types';
import { relay } from '$lib/server/solana';
import { allow, clientKey } from '$lib/server/rateLimit';

// Each relayed tx costs the treasury fees (and sometimes rent), so bound it:
// a full demo trade is a handful of transactions.
const PER_IP = { limit: 30, windowMs: 10 * 60 * 1000 };
const GLOBAL = { limit: 300, windowMs: 10 * 60 * 1000 };

/** Fee sponsorship: co-sign a client-built, role-signed tx with the treasury and submit it. */
export const POST: RequestHandler = async (event) => {
	if (
		!allow(`relay:${clientKey(event)}`, PER_IP.limit, PER_IP.windowMs) ||
		!allow('relay:global', GLOBAL.limit, GLOBAL.windowMs)
	) {
		return json({ error: 'Too many transactions; try again later' }, { status: 429 });
	}
	const { tx } = (await event.request.json()) as { tx?: string };
	if (!tx) throw error(400, 'Missing tx');
	try {
		const signature = await relay(tx);
		return json({ signature });
	} catch (e) {
		return json({ error: e instanceof Error ? e.message : String(e) }, { status: 500 });
	}
};
