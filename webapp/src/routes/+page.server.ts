import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async ({ locals }) => {
	return {
		// Populated by src/hooks.server.ts from the IAP identity header.
		userEmail: locals.userEmail
	};
};
