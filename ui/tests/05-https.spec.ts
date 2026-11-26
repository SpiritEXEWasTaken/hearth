import { test, expect } from '@playwright/test';
import { signIn, apiAs, waitJob } from './helpers';

// A real Home Assistant behind Hearth's HTTPS port. VM only (HEARTH_SYSTEM_URL); the VM forwards
// 8443 to the host so the browser can open the app's HTTPS address.
test.skip(!process.env.HEARTH_SYSTEM_URL, 'HEARTH_SYSTEM_URL not set');
test.describe.configure({ mode: 'serial' });

const HA = `x-hearth:
  title: Home Assistant
  web: { port: 8123 }
services:
  homeassistant:
    image: ghcr.io/home-assistant/home-assistant:stable
    network_mode: host
    volumes:
      - /srv/ha-test/config:/config
    restart: unless-stopped
`;

test('Home Assistant over HTTPS: turn on, fix the proxy trust, open it', async ({ page, browser }) => {
	test.setTimeout(1_200_000);
	await signIn(page);

	const apps = (await (await page.request.get('/v1/apps')).json()).data as { id: string }[];
	if (!apps.some((a) => a.id === 'homeassistant')) {
		const r = await apiAs(page, 'POST', '/v1/apps', { app: 'homeassistant', compose: HA });
		expect(r.ok, JSON.stringify(r)).toBeTruthy();
		const job = await waitJob(page.request, r.data.job);
		expect(job.status, JSON.stringify(job)).toBe('succeeded');
	}

	await page.goto('/apps/homeassistant');
	const card = page.locator('.card').filter({ has: page.getByRole('heading', { name: 'HTTPS' }) });
	await expect(card.getByRole('button', { name: /Use HTTPS|Turn off/ })).toBeVisible();
	if (await card.getByRole('button', { name: 'Turn off' }).isVisible()) {
		await card.getByRole('button', { name: 'Turn off' }).click();
	}
	await card.getByLabel('Port').fill('8443');
	await card.getByRole('button', { name: 'Use HTTPS' }).click();
	await expect(card.getByRole('link', { name: /:8443$/ }).first()).toBeVisible();

	// HA needs a minute on its first start; test until it answers (400 = it refuses the proxy).
	let result = '';
	for (let i = 0; i < 60; i++) {
		await card.getByRole('button', { name: 'Test' }).click();
		const status = card.getByRole('status');
		await expect(status).toBeVisible();
		result = (await status.textContent()) ?? '';
		if (!/doesn't answer/.test(result)) break;
		await page.waitForTimeout(5000);
	}
	expect(result).toContain('refuses requests through a proxy');

	await card.getByRole('button', { name: 'Fix it' }).click();
	const d = page.getByRole('dialog', { name: /Let Home Assistant work through HTTPS/ });
	await expect(d).toContainText('trusted_proxies');
	await d.getByRole('button', { name: 'Change and restart' }).click();
	await expect(page.getByRole('status').filter({ hasText: /Done\.|didn't work|failed/ })).toHaveText('Done.', {
		timeout: 300_000
	});
	await page.getByRole('button', { name: 'Close' }).last().click();
	for (let i = 0; i < 40; i++) {
		await card.getByRole('button', { name: 'Test' }).click();
		result = (await card.getByRole('status').textContent()) ?? '';
		if (result.includes('Works')) break;
		await page.waitForTimeout(3000);
	}
	expect(result).toContain('Works');
	await page.screenshot({ path: 'test-results/https-card.png', fullPage: true });

	// The app itself over HTTPS, websocket included.
	const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
	const ha = await ctx.newPage();
	await ha.goto('https://127.0.0.1:8443/');
	await expect(ha).toHaveTitle(/Home Assistant/, { timeout: 60_000 });
	const ws = await ha.evaluate(
		() =>
			new Promise<string>((resolve) => {
				const s = new WebSocket(`wss://${location.host}/api/websocket`);
				s.onmessage = (m) => {
					resolve(String(m.data));
					s.close();
				};
				s.onerror = () => resolve('error');
				setTimeout(() => resolve('timeout'), 15000);
			})
	);
	expect(ws).toContain('auth_required');
	await ha.screenshot({ path: 'test-results/ha-over-https.png' });
	await ctx.close();
});
