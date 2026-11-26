import { test, expect } from '@playwright/test';
import { login } from './helpers';

test('phone: bottom bar, tiles fit, app page usable', async ({ page }) => {
	await login(page);
	const bar = page.locator('nav.bottombar');
	await expect(bar).toBeVisible();
	await expect(page.locator('aside.sidebar')).toBeHidden();
	// Nothing wider than the screen.
	const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
	expect(overflow).toBeLessThanOrEqual(0);
	await page.screenshot({ path: 'test-results/phone-dashboard.png', fullPage: true });

	await bar.getByRole('link', { name: 'Apps' }).click();
	await page.getByRole('link', { name: /Ticker/ }).click();
	await expect(page.getByRole('tab', { name: 'Logs' })).toBeVisible();
	await page.getByRole('tab', { name: 'Logs' }).click();
	await expect(page.getByRole('log')).toContainText('tick');
	const overflow2 = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
	expect(overflow2).toBeLessThanOrEqual(0);
	await page.screenshot({ path: 'test-results/phone-app.png', fullPage: true });

	await bar.getByRole('link', { name: 'System' }).click();
	await expect(page.getByRole('tab', { name: 'Packages' })).toBeVisible();
	await expect(page.getByText(/kernel/)).toBeVisible();
	const overflow3 = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
	expect(overflow3).toBeLessThanOrEqual(0);

	await bar.getByRole('link', { name: 'Settings' }).click();
	await expect(page.getByRole('heading', { name: 'Your account' })).toBeVisible();
});
