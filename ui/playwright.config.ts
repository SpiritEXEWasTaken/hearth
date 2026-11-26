import { defineConfig, devices } from '@playwright/test';

// Runs against a live Hearth (see tests/README.md): HEARTH_E2E_URL=https://127.0.0.1:9443
export default defineConfig({
	testDir: 'tests',
	fullyParallel: false,
	workers: 1,
	timeout: 60_000,
	expect: { timeout: 15_000 },
	reporter: [['list']],
	use: {
		baseURL: process.env.HEARTH_E2E_URL ?? 'https://127.0.0.1:9443',
		ignoreHTTPSErrors: true,
		channel: process.env.PW_CHANNEL || undefined,
		trace: 'off',
		screenshot: 'only-on-failure'
	},
	projects: [
		{ name: 'setup', testMatch: /01-setup\.spec\.ts/ },
		{
			name: 'desktop',
			testMatch: /02-app\.spec\.ts/,
			dependencies: ['setup'],
			use: { viewport: { width: 1280, height: 860 } }
		},
		{
			// Real system updates and packages: run against a disposable VM with
			// HEARTH_SYSTEM_URL (and HEARTH_SYSTEM_SETUP_CODE on its first run).
			name: 'system',
			testMatch: /04-system\.spec\.ts/,
			use: { baseURL: process.env.HEARTH_SYSTEM_URL, viewport: { width: 1280, height: 860 } }
		},
		{
			name: 'phone',
			testMatch: /03-phone\.spec\.ts/,
			dependencies: ['desktop'],
			// 360 px wide: the narrowest common Android phone.
			use: { ...devices['Pixel 5'], viewport: { width: 360, height: 760 }, channel: process.env.PW_CHANNEL || undefined }
		}
	]
});
