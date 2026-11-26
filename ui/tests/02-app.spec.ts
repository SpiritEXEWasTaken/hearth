import { test, expect } from '@playwright/test';
import { ADMIN, VIEWER, login } from './helpers';

test.describe.configure({ mode: 'serial' });

test('dashboard shows widgets, tiles and other apps', async ({ page }) => {
	await login(page);
	await expect(page.getByText('CPU', { exact: true })).toBeVisible();
	await expect(page.getByText('Memory', { exact: true })).toBeVisible();
	await expect(page.getByText('Storage', { exact: true })).toBeVisible();
	const tile = page.getByRole('link', { name: /^Open Whoami/ });
	await expect(tile).toBeVisible();
	await expect(tile).toHaveAttribute('href', /:18081\//);
	await expect(page.getByRole('heading', { name: 'Other apps and services' })).toBeVisible();
	await expect(page.getByRole('link', { name: /Ticker/ })).toBeVisible();
	await page.screenshot({ path: 'test-results/desktop-dashboard.png', fullPage: true });
});

test('apps list filters and opens an app', async ({ page }) => {
	await login(page);
	await page.getByRole('navigation', { name: 'Main' }).first().getByRole('link', { name: 'Apps' }).click();
	await page.getByLabel('Search apps').fill('tick');
	await expect(page.getByRole('link', { name: /Ticker/ })).toBeVisible();
	await expect(page.getByRole('link', { name: /Whoami/ })).toHaveCount(0);
	await page.getByRole('link', { name: /Ticker/ }).click();
	await expect(page.getByRole('heading', { name: 'Ticker' })).toBeVisible();
	await expect(page.getByRole('heading', { name: 'Containers' })).toBeVisible();
});

test('logs tab shows live lines and search', async ({ page }) => {
	await login(page);
	await page.goto('/apps/ticker?tab=logs');
	await expect(page.getByRole('log')).toContainText('tick');
	await page.getByLabel('Search the log').fill('no-such-text-xyz');
	await expect(page.getByRole('log')).toContainText('Nothing matches.');
});

test('stop and start an app with live job progress', async ({ page }) => {
	await login(page);
	await page.goto('/apps/ticker');
	await page.getByRole('button', { name: 'Stop' }).click();
	await expect(page.getByRole('status')).toHaveText('Done.', { timeout: 60_000 });
	await page.getByRole('button', { name: 'Close' }).last().click();
	await expect(page.getByText('Stopped').first()).toBeVisible();
	await page.getByRole('button', { name: 'Start' }).click();
	await expect(page.getByRole('status')).toHaveText('Done.', { timeout: 60_000 });
	await page.getByRole('button', { name: 'Close' }).last().click();
	await expect(page.getByText('Running').first()).toBeVisible();
});

test('settings tab: edit compose, review the diff, apply, then go back in history', async ({ page }) => {
	await login(page);
	await page.goto('/apps/ticker?tab=settings');
	const editor = page.getByLabel('Compose file');
	await expect(editor).toHaveValue(/busybox/);
	const text = await editor.inputValue();
	await editor.fill(text.replace('sleep 2', 'sleep 3'));
	await page.getByPlaceholder(/What did you change/).fill('slower ticks');
	await page.getByRole('button', { name: 'Review changes' }).click();
	const dialog = page.getByRole('dialog', { name: 'Review changes' });
	await expect(dialog).toContainText('sleep 3');
	await dialog.getByRole('button', { name: 'Apply' }).click();
	await expect(page.getByRole('status')).toHaveText('Done.', { timeout: 120_000 });
	await page.getByRole('button', { name: 'Close' }).last().click();

	await page.getByRole('tab', { name: 'History' }).click();
	await expect(page.getByText('slower ticks', { exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Go back to this' }).first().click();
	await expect(page.getByRole('dialog')).toContainText('sleep 2');
	await page.getByRole('dialog').getByRole('button', { name: 'Go back' }).click();
	await expect(page.getByRole('status')).toHaveText('Done.', { timeout: 120_000 });
});

test('backups tab: back up and list', async ({ page }) => {
	await login(page);
	await page.goto('/apps/ticker?tab=backups');
	await page.getByRole('button', { name: 'Back up', exact: true }).click();
	await expect(page.getByRole('status')).toHaveText('Done.', { timeout: 120_000 });
	await page.getByRole('button', { name: 'Close' }).last().click();
	await expect(page.getByRole('button', { name: 'Restore' }).first()).toBeVisible();
});

test('updates tab checks the registry', async ({ page }) => {
	await login(page);
	await page.goto('/apps/whoami?tab=updates');
	await page.getByRole('button', { name: 'Check now' }).click();
	await expect(page.getByText(/Last checked (just now|\d+ min ago)/)).toBeVisible({ timeout: 60_000 });
});

test('HTTPS for an app: turn on, test, open through the proxy, turn off', async ({ page, browser }) => {
	await login(page);
	await page.goto('/apps/whoami');
	const card = page.locator('.card').filter({ has: page.getByRole('heading', { name: 'HTTPS' }) });
	await expect(card.getByRole('button', { name: 'Use HTTPS' })).toBeVisible();
	await card.getByLabel('Port').fill('18443');
	await card.getByRole('button', { name: 'Use HTTPS' }).click();
	await expect(card.getByRole('status')).toContainText('Works');
	const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
	const p2 = await ctx.newPage();
	await p2.goto('https://127.0.0.1:18443/some/path?x=1');
	const body = (await p2.textContent('body')) ?? '';
	expect(body).toContain('GET /some/path?x=1');
	expect(body).toMatch(/X-Forwarded-Proto: https/i);
	await ctx.close();
	// The tile now opens the HTTPS address.
	await page.goto('/');
	await expect(page.getByRole('link', { name: /^Open Whoami/ })).toHaveAttribute('href', /^https:\/\/.*:18443\//);
	await page.goto('/apps/whoami');
	await card.getByRole('button', { name: 'Turn off' }).click();
	await expect(card.getByRole('button', { name: 'Use HTTPS' })).toBeVisible();
});

test('users: add a read-only user who cannot change apps', async ({ page, browser }) => {
	await login(page);
	await page.goto('/settings');
	await expect(page.getByRole('heading', { name: 'HTTPS' })).toBeVisible();
	await page.getByRole('button', { name: 'Add user' }).click();
	const d = page.getByRole('dialog', { name: 'Add a user' });
	await d.getByLabel('Username').fill(VIEWER.username);
	await d.getByLabel('Password').fill(VIEWER.password);
	await d.getByRole('button', { name: 'Add' }).click();
	await expect(page.getByRole('cell', { name: VIEWER.username, exact: true })).toBeVisible();

	const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
	const p2 = await ctx.newPage();
	await login(p2, VIEWER);
	await p2.goto('/apps/ticker');
	await expect(p2.getByRole('heading', { name: 'Ticker' })).toBeVisible();
	await expect(p2.getByRole('button', { name: 'Stop' })).toHaveCount(0);
	await p2.goto('/apps/ticker?tab=settings');
	await expect(p2.getByText('only admins can change it')).toBeVisible();
	await p2.goto('/settings');
	await expect(p2.getByRole('heading', { name: 'Users' })).toHaveCount(0);
	await ctx.close();
});

test('wrong password is refused, sign out works', async ({ page }) => {
	await page.goto('/login');
	await page.getByLabel('Username').fill(ADMIN.username);
	await page.getByLabel('Password').fill('not-the-password');
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page.getByRole('alert')).toContainText('wrong username or password');
	await login(page);
	await page.getByRole('button', { name: ADMIN.username }).click();
	await page.getByRole('menuitem', { name: 'Sign out' }).click();
	await expect(page).toHaveURL(/\/login/);
	await page.goto('/apps');
	await expect(page).toHaveURL(/\/login\?next=%2Fapps/);
});
