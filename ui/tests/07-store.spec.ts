import { test, expect } from '@playwright/test';
import { login, apiAs, waitJob } from './helpers';

// Uses the signed test catalog from ci/store-fixture (served by ci/e2e.sh).
test.describe.configure({ mode: 'serial' });

test('store lists the catalog, searches and filters', async ({ page }) => {
	await login(page);
	await apiAs(page, 'POST', '/v1/store/refresh', {});
	await page.goto('/store');
	await expect(page.getByTestId('store-card')).toHaveCount(2);
	await expect(page.getByText('signed and checked by Hearth')).toBeVisible();
	await page.getByLabel('Search the store').fill('request');
	await expect(page.getByTestId('store-card')).toHaveCount(1);
	await page.getByLabel('Search the store').fill('');
	await page.getByRole('button', { name: 'Network & DNS' }).click();
	await expect(page.getByTestId('store-card')).toHaveCount(1);
	await expect(page.getByTestId('store-card')).toContainText('Port Clash');
});

test('a port clash blocks the install and says why', async ({ page }) => {
	await login(page);
	await page.goto('/store/org.example.dnsblock');
	await expect(page.getByTestId('host-checks')).toContainText('port 18081/tcp is already in use');
	await expect(page.getByTestId('install-button')).toBeDisabled();
	await expect(page.getByTestId('install-button')).toContainText("Can't install");
});

test('install a store app with an option, then see its notes and sign-in details', async ({ page }) => {
	await login(page);
	await page.goto('/store/org.example.whoami');
	await expect(page.getByRole('heading', { name: 'Whoami' })).toBeVisible();
	await expect(page.getByTestId('content-notice')).toContainText('No test data');
	await expect(page.getByRole('img', { name: 'The test page' })).toBeVisible();
	// "whoami" is taken by the test app, so the store suggests another name.
	await expect(page.getByLabel('Name on this server')).toHaveValue('whoami-2');
	await page.getByLabel('Verbose logging').check();
	await expect(page.getByTestId('host-checks')).toContainText('Web page: port 18082/tcp is free');
	await page.getByTestId('install-button').click();
	await page.waitForURL(/\/apps\/whoami-2\?installed=1/, { timeout: 180_000 });
	const card = page.getByTestId('store-card');
	await expect(card).toContainText('Installed. Here');
	await expect(card).toContainText('Open the page to see your request.');
	await card.getByRole('button', { name: 'Show sign-in details' }).click();
	await expect(card).toContainText('Secret word');
	const compose = await apiAs(page, 'GET', '/v1/apps/whoami-2/compose');
	expect(JSON.stringify(compose)).toContain('--verbose');
	await page.goto('/store');
	await expect(page.getByTestId('store-card').filter({ hasText: 'Whoami' })).toContainText('Installed');
});

test('remove the store app again', async ({ page }) => {
	await login(page);
	const r = await apiAs(page, 'POST', '/v1/apps/whoami-2/uninstall', { delete_data: true });
	const job = await waitJob(page.request, r.data.job);
	expect(job.status).toBe('succeeded');
});
