import { test, expect } from '@playwright/test';
import { login } from './helpers';

// Your own domain against Pebble (Let's Encrypt's test server) and its fake DNS: only when the
// daemon was started with HEARTH_ACME_DIRECTORY etc. (ci/e2e.sh does that).
test.skip(!process.env.HEARTH_PEBBLE, 'HEARTH_PEBBLE not set');
test.describe.configure({ mode: 'serial' });

test('own domain: set up, get certificates, give an app a subdomain, open it', async ({ page, browser }) => {
	test.setTimeout(300_000);
	await login(page);
	await page.goto('/settings');
	await page.getByPlaceholder('yourname').fill('testhome');
	await page.getByLabel(/DuckDNS token/).fill('00000000-0000-4000-8000-000000000000');
	// CI machines have no Tailscale; every machine has a home-network-style address.
	await page.getByLabel('The name points to').selectOption('lan');
	await page.getByRole('button', { name: 'Save and get certificates' }).click();
	const status = page.getByRole('dialog').getByRole('status');
	await expect(status).not.toHaveText('Working…', { timeout: 180_000 });
	await expect(status).toHaveText('Done.');
	await page.getByRole('button', { name: 'Close' }).last().click();
	await expect(page.getByText('*.testhome.duckdns.org', { exact: true })).toBeVisible();
	await expect(page.getByRole('link', { name: 'https://testhome.duckdns.org' })).toBeVisible();

	await page.goto('/apps/whoami');
	const card = page.locator('.card').filter({ has: page.getByRole('heading', { name: 'HTTPS' }) });
	await expect(card.getByLabel('Subdomain')).toHaveValue('whoami');
	await card.getByLabel('Port').fill('18445');
	await card.getByRole('button', { name: 'Use HTTPS' }).click();
	await expect(card.getByRole('link', { name: 'https://whoami.testhome.duckdns.org/' })).toBeVisible();

	// The browser resolves *.testhome.duckdns.org to this machine (see playwright.config.ts).
	const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
	const p2 = await ctx.newPage();
	await p2.goto('https://whoami.testhome.duckdns.org:9443/via-domain');
	await expect(p2.locator('body')).toContainText('GET /via-domain');
	await p2.goto('https://nothing.testhome.duckdns.org:9443/');
	await expect(p2.getByRole('heading', { name: 'No app uses this address' })).toBeVisible();
	await ctx.close();

	await page.goto('/apps/whoami');
	await card.getByRole('button', { name: 'Turn off' }).click();
	await page.goto('/settings');
	await page.getByRole('button', { name: 'Remove', exact: true }).click();
	await page.getByRole('dialog').getByRole('button', { name: 'Remove' }).click();
	await expect(page.getByRole('button', { name: 'Save and get certificates' })).toBeVisible();
});
