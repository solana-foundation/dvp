import { json, error } from '@sveltejs/kit';
import type { RequestHandler } from './$types';
import { fundRoles } from '$lib/server/solana';
import type { RoleKey } from '$lib/config';

export const POST: RequestHandler = async ({ request }) => {
	const { addresses } = (await request.json()) as { addresses?: Record<RoleKey, string> };
	if (!addresses?.maker || !addresses?.partyA || !addresses?.partyB || !addresses?.authority) {
		throw error(400, 'Missing role addresses');
	}
	try {
		const out = await fundRoles(addresses);
		return json(out);
	} catch (e) {
		return json({ error: e instanceof Error ? e.message : String(e) }, { status: 500 });
	}
};
