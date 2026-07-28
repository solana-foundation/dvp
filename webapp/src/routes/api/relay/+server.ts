import { json, error } from '@sveltejs/kit';
import type { RequestHandler } from './$types';
import { relay } from '$lib/server/solana';

/** Fee sponsorship: co-sign a client-built, role-signed tx with the treasury and submit it. */
export const POST: RequestHandler = async ({ request }) => {
	const { tx } = (await request.json()) as { tx?: string };
	if (!tx) throw error(400, 'Missing tx');
	try {
		const signature = await relay(tx);
		return json({ signature });
	} catch (e) {
		return json({ error: e instanceof Error ? e.message : String(e) }, { status: 500 });
	}
};
