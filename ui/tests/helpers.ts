import { expect, type Page, type APIRequestContext } from '@playwright/test';

export const ADMIN = { username: 'admin', password: 'correct-horse-battery' };
export const VIEWER = { username: 'family', password: 'family-password-1' };

export async function login(page: Page, who = ADMIN) {
	await page.goto('/login');
	await page.getByLabel('Username').fill(who.username);
	await page.getByLabel('Password').fill(who.password);
	await page.getByRole('button', { name: 'Sign in' }).click();
	await expect(page.getByRole('heading', { name: 'Apps', exact: true })).toBeVisible();
}

/** Calls the API with the page's session (cookie + CSRF header). */
export async function apiAs(page: Page, method: string, path: string, body?: unknown) {
	const me = await (await page.request.get('/v1/auth/me')).json();
	const res = await page.request.fetch(path, {
		method,
		headers: { 'x-hearth-csrf': me.data.csrf, 'content-type': 'application/json' },
		data: body === undefined ? undefined : JSON.stringify(body)
	});
	return res.json();
}

export async function waitJob(request: APIRequestContext, id: string) {
	for (let i = 0; i < 240; i++) {
		const j = (await (await request.get(`/v1/jobs/${id}`)).json()).data;
		if (j.status !== 'running') return j;
		await new Promise((r) => setTimeout(r, 1000));
	}
	throw new Error('job did not finish');
}

/** Signs in, or creates the admin on a fresh server (HEARTH_SYSTEM_SETUP_CODE). */
export async function signIn(page: Page) {
	await page.goto('/');
	const welcome = page.getByRole('heading', { name: 'Welcome to Hearth' });
	const signin = page.getByRole('button', { name: 'Sign in' });
	const home = page.getByRole('heading', { name: 'Apps', exact: true });
	await expect(welcome.or(signin).or(home)).toBeVisible();
	if (await welcome.isVisible()) {
		await page.getByLabel('Setup code').fill(process.env.HEARTH_SYSTEM_SETUP_CODE ?? '');
		await page.getByLabel('Username').fill(ADMIN.username);
		await page.locator('input[name=password]').fill(ADMIN.password);
		await page.getByLabel('Password again').fill(ADMIN.password);
		await page.getByRole('button', { name: 'Create admin account' }).click();
	} else if (await signin.isVisible()) {
		await page.getByLabel('Username').fill(ADMIN.username);
		await page.getByLabel('Password').fill(ADMIN.password);
		await signin.click();
	}
	await expect(home).toBeVisible();
}

