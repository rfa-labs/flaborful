import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

// Static SPA, served by the axum binary (ServeDir + SPA fallback) — the same
// single-image pattern as UMS and optimizer: one Docker image, one Fly app, the
// frontend on the same origin as /api/v1. Same-origin is what lets the API client
// keep its relative `BASE = '/api/v1'` and keeps CORS out of the picture.
//
// adapter-static emits a plain build/ folder (no Node server). `fallback` makes it
// an SPA shell so client-routed deep links (/studio/jobs/:id) resolve to the app
// instead of 404ing at the file server.
/** @type {import('@sveltejs/kit').Config} */
const config = {
	preprocess: vitePreprocess(),
	kit: {
		adapter: adapter({
			fallback: '200.html',
			precompress: false
		})
	}
};

export default config;
