import { test, expect } from '@playwright/test';
import { ADMIN, apiAs, waitJob } from './helpers';

// Fresh Hearth: first-run setup creates the admin, then two small test apps are installed.
test('first-run setup', async ({ page }) => {
	const code = process.env.HEARTH_SETUP_CODE;
	test.skip(!code, 'HEARTH_SETUP_CODE not set (Hearth already set up)');

	await page.goto('/');
	await expect(page).toHaveURL(/\/setup$/);
	await expect(page.getByRole('heading', { name: 'Welcome to Hearth' })).toBeVisible();

	// A wrong code is refused.
	await page.getByLabel('Setup code').fill('AAAA-BBBB');
	await page.getByLabel('Username').fill(ADMIN.username);
	await page.locator('input[name=password]').fill(ADMIN.password);
	await page.getByLabel('Password again').fill(ADMIN.password);
	await page.getByRole('button', { name: 'Create admin account' }).click();
	await expect(page.getByRole('alert')).toContainText('wrong setup code');

	await page.getByLabel('Setup code').fill(code!.toLowerCase());
	await page.getByRole('button', { name: 'Create admin account' }).click();
	await expect(page.getByRole('heading', { name: 'Apps', exact: true })).toBeVisible();

	// Test apps: a web app on the dashboard and a background one.
	const web = `x-hearth:
  title: Whoami
  web: { port: 18081 }
services:
  whoami:
    image: traefik/whoami:latest
    ports: ["18081:80"]
    restart: unless-stopped
`;
	const bg = `x-hearth:
  title: Ticker
services:
  ticker:
    image: busybox:latest
    command: ["sh", "-c", "while true; do echo tick $$(date +%s); sleep 2; done"]
    restart: unless-stopped
`;
	for (const [app, compose] of [
		['whoami', web],
		['ticker', bg]
	]) {
		const r = await apiAs(page, 'POST', '/v1/apps', { app, compose });
		expect(r.ok, JSON.stringify(r)).toBeTruthy();
		const job = await waitJob(page.request, r.data.job);
		expect(job.status, JSON.stringify(job)).toBe('succeeded');
	}
});
