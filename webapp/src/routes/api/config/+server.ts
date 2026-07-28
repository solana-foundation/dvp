import { json } from '@sveltejs/kit';
import type { RequestHandler } from './$types';
import { PROGRAM_ID, CLUSTER, ASSET_TOKEN, CASH_TOKEN, PRESET } from '$lib/config';
import { mintAddresses } from '$lib/server/solana';

export const GET: RequestHandler = async () => {
	let mints: { asset: string; cash: string } | null = null;
	try {
		mints = await mintAddresses();
	} catch {
		mints = null;
	}
	return json({
		programId: PROGRAM_ID,
		cluster: CLUSTER,
		mints,
		tokens: { asset: ASSET_TOKEN, cash: CASH_TOKEN },
		presets: {
			amountA: PRESET.amountA.toString(),
			amountB: PRESET.amountB.toString(),
			expirySeconds: PRESET.expirySeconds
		}
	});
};
