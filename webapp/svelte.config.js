import adapter from '@sveltejs/adapter-node';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	preprocess: vitePreprocess(),
	kit: {
		adapter: adapter(),
		paths: {
			// The platform serves this app at https://{user}.{base_domain}/{app}/.
			// Cloud Build injects BASE_PATH=/{app} as a Docker build arg.
			// Locally BASE_PATH is unset, so the app runs at /.
			base: process.env.BASE_PATH ?? ''
		}
	}
};

export default config;
