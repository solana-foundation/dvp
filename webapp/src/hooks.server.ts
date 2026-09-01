import type { Handle } from '@sveltejs/kit';
import { env } from '$env/dynamic/private';

/**
 * Google IAP fronts every SSO-gated app and sets
 * `X-Goog-Authenticated-User-Email: accounts.google.com:user@solana.org`.
 *
 * Trust boundary: the header is only trustworthy while IAP is enabled. The
 * platform injects APP_VISIBILITY ("sso" | "public") into every revision;
 * when it is "public", IAP is off and the load balancer forwards a
 * client-supplied header untouched — attacker-controlled, so ignore it.
 * (env is read dynamically because visibility can change per revision
 * without a rebuild.) Locally the header is simply absent.
 */
export const handle: Handle = async ({ event, resolve }) => {
	if (env.APP_VISIBILITY === 'public') {
		event.locals.userEmail = null;
	} else {
		const raw = event.request.headers.get('x-goog-authenticated-user-email');
		event.locals.userEmail = raw ? raw.replace(/^accounts\.google\.com:/, '') : null;
	}
	return resolve(event);
};
