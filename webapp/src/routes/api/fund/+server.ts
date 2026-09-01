import { json, error } from '@sveltejs/kit';
import { address } from '@solana/kit';
import type { RequestHandler } from './$types';
import { fundRoles } from '$lib/server/solana';
import type { RoleKey } from '$lib/config';
import { allow, clientKey } from '$lib/server/rateLimit';

// Every call spends treasury funds (token mints, ATA rent, fees), so bound it
// hard: a demo session needs one call plus the odd retry, and the instance-wide
// cap stops a distributed drain across many source IPs.
const PER_IP = { limit: 5, windowMs: 60 * 60 * 1000 };
const GLOBAL = { limit: 60, windowMs: 60 * 60 * 1000 };

export const POST: RequestHandler = async (event) => {
	if (
		!allow(`fund:${clientKey(event)}`, PER_IP.limit, PER_IP.windowMs) ||
		!allow('fund:global', GLOBAL.limit, GLOBAL.windowMs)
	) {
		return json({ error: 'Too many funding requests; try again later' }, { status: 429 });
	}
	const { addresses } = (await event.request.json()) as { addresses?: Record<RoleKey, string> };
	if (!addresses?.maker || !addresses?.partyA || !addresses?.partyB || !addresses?.authority) {
		throw error(400, 'Missing role addresses');
	}
	try {
		address(addresses.partyA);
		address(addresses.partyB);
	} catch {
		throw error(400, 'Invalid role address');
	}
	try {
		const out = await fundRoles(addresses);
		return json(out);
	} catch (e) {
		return json({ error: e instanceof Error ? e.message : String(e) }, { status: 500 });
	}
};
