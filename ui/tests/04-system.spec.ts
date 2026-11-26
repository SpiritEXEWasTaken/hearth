import { test, expect, type Page } from '@playwright/test';
import { signIn } from './helpers';

// Changes packages for real: only against a throwaway VM (HEARTH_SYSTEM_URL).
test.skip(!process.env.HEARTH_SYSTEM_URL, 'HEARTH_SYSTEM_URL not set');
test.describe.configure({ mode: 'serial' });

async function closeJob(page: Page) {
	await expect(page.getByRole('status').filter({ hasText: /Done\.|didn't work|failed/i })).toHaveText('Done.', { timeout: 600_000 });
	await page.getByRole('button', { name: 'Close' }).last().click();
}

test('system updates: check, list, schedule setting', async ({ page }) => {
	test.setTimeout(600_000);
	await signIn(page);
	await page.getByRole('link', { name: 'System' }).first().click();
	await expect(page.getByText(/kernel/)).toBeVisible();
	await page.getByRole('button', { name: 'Check now' }).click();
	await closeJob(page);
	await expect(page.getByRole('heading', { name: /Up to date|update(s)? available/ })).toBeVisible();
	await expect(page.getByText('System packages', { exact: true })).toBeVisible();
	await page.getByLabel('Automatic security updates').selectOption('daily');
	await page.getByRole('button', { name: 'Save' }).click();
	await expect(page.getByRole('status').filter({ hasText: 'Saved' })).toContainText('every day');
	await page.getByLabel('Automatic security updates').selectOption('off');
	await page.getByRole('button', { name: 'Save' }).click();
	await expect(page.getByRole('status').filter({ hasText: 'Saved' })).toContainText('only listed');
	await page.screenshot({ path: 'test-results/system-updates.png', fullPage: true });
});

test('packages: install and remove a small package with a plan first', async ({ page }) => {
	test.setTimeout(600_000);
	await signIn(page);
	await page.goto('/system?tab=packages');
	await page.getByLabel('Search packages').fill('sl');
	await page.getByLabel('Where to search').selectOption('native');
	await page.getByRole('button', { name: 'Search', exact: true }).click();
	const row = page.getByRole('button', { name: /^sl\s/ }).first();
	await expect(row).toBeVisible({ timeout: 60_000 });
	await row.click();
	await page.getByRole('button', { name: 'Install…' }).click();
	const plan = page.getByRole('dialog', { name: 'Install these packages?' });
	await expect(plan).toContainText('sl');
	await plan.getByRole('button', { name: 'Install' }).click();
	await closeJob(page);

	await page.getByRole('button', { name: 'Search', exact: true }).click();
	await expect(page.getByRole('button', { name: /^sl\s.*Installed/ }).first()).toBeVisible({ timeout: 60_000 });
	await page.getByRole('button', { name: /^sl\s/ }).first().click();
	await page.getByRole('button', { name: 'Remove…' }).click();
	const rm = page.getByRole('dialog', { name: 'Remove these packages?' });
	await expect(rm).toContainText('sl');
	await rm.getByRole('button', { name: 'Remove' }).click();
	await closeJob(page);
});

test('packages: removing something critical needs an explicit tick', async ({ page }) => {
	await signIn(page);
	await page.goto('/system?tab=packages');
	await page.getByLabel('Search packages').fill('openssh-server');
	await page.getByLabel('Where to search').selectOption('native');
	await page.getByRole('button', { name: 'Search', exact: true }).click();
	await page.getByRole('button', { name: /^openssh-server\s/ }).first().click();
	await page.getByRole('button', { name: 'Remove…' }).click();
	const rm = page.getByRole('dialog', { name: 'Remove these packages?' });
	await expect(rm.getByRole('button', { name: 'Remove' })).toBeDisabled();
	await expect(rm.getByText('I understand')).toBeVisible();
	await rm.getByRole('button', { name: 'Cancel' }).click();
});

test('system updates: install the security updates through the plan dialog', async ({ page }) => {
	test.setTimeout(900_000);
	await signIn(page);
	await page.goto('/system');
	await expect(page.getByRole('heading', { name: /Up to date|update(s)? available/ })).toBeVisible();
	const btn = page.getByRole('button', { name: /Install \d+ security update/ });
	test.skip(!(await btn.isVisible().catch(() => false)), 'no security updates to install');
	await btn.click();
	const d = page.getByRole('dialog', { name: 'Install these updates?' });
	await expect(d).toContainText('will be installed');
	await d.getByRole('button', { name: 'Install' }).click();
	await closeJob(page);
	await expect(page.getByRole('button', { name: /Install \d+ security update/ })).toHaveCount(0);
});
