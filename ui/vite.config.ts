import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [
		sveltekit({
			// A single-page app embedded in the hearth binary: every route loads index.html.
			adapter: adapter({ fallback: 'index.html', strict: false }),
			csp: {
				mode: 'hash',
				directives: {
					'default-src': ['self'],
					'script-src': ['self'],
					'style-src': ['self', 'unsafe-inline'],
					'img-src': ['self', 'data:'],
					'connect-src': ['self'],
					'font-src': ['self'],
					'object-src': ['none'],
					'base-uri': ['none']
				}
			},
			version: { pollInterval: 0 }
		})
	],
	server: {
		// `npm run dev` against a running Hearth: HEARTH_URL=https://jarvis.example.ts.net npm run dev
		proxy: process.env.HEARTH_URL
			? {
					'/v1': { target: process.env.HEARTH_URL, changeOrigin: true, secure: false },
					'/ca.crt': { target: process.env.HEARTH_URL, changeOrigin: true, secure: false }
				}
			: undefined
	}
});
