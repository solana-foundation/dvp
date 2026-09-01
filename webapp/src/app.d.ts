// See https://svelte.dev/docs/kit/types#app.d.ts
declare global {
	namespace App {
		interface Locals {
			/**
			 * Email of the signed-in user, taken from the IAP header
			 * `X-Goog-Authenticated-User-Email` in src/hooks.server.ts.
			 * `null` in local dev and when the app's visibility is "public".
			 */
			userEmail: string | null;
		}
		// interface Error {}
		// interface PageData {}
		// interface PageState {}
		// interface Platform {}
	}
}

export {};
