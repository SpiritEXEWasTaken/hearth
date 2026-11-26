<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { get, post, whenSignedOut, errorText } from '#lib/api.ts';
	import { session, theme } from '#lib/session.svelte.ts';
	import type { Me } from '#lib/types.ts';
	import Icon from '#lib/components/Icon.svelte';

	let { children } = $props();
	let ready = $state(false);
	let fatal = $state('');
	let menuOpen = $state(false);

	const publicPages = ['/login', '/setup'];
	const isPublic = $derived(publicPages.includes(page.url.pathname));

	async function check() {
		try {
			const s = await get<{ needed: boolean; hostname: string }>('/v1/setup');
			session.hostname = s.hostname;
			if (s.needed) {
				session.set(null);
				if (page.url.pathname !== '/setup') await goto('/setup', { replace: true });
				return;
			}
			try {
				session.set(await get<Me>('/v1/auth/me'));
				if (isPublic) await goto('/', { replace: true });
			} catch {
				session.set(null);
				if (!isPublic || page.url.pathname === '/setup') {
					const next = page.url.pathname === '/' || isPublic ? '' : `?next=${encodeURIComponent(page.url.pathname)}`;
					await goto(`/login${next}`, { replace: true });
				}
			}
		} catch (e) {
			fatal = errorText(e);
		} finally {
			ready = true;
		}
	}

	onMount(() => {
		theme.init();
		whenSignedOut(() => {
			session.set(null);
			if (!publicPages.includes(location.pathname)) goto('/login');
		});
		check();
	});

	async function signOut() {
		menuOpen = false;
		try {
			await post('/v1/auth/logout');
		} catch {
			/* signed out anyway */
		}
		session.set(null);
		await goto('/login');
	}

	const nav = [
		{ href: '/', label: 'Home', icon: 'home' },
		{ href: '/apps', label: 'Apps', icon: 'apps' },
		{ href: '/system', label: 'System', icon: 'shield' },
		{ href: '/settings', label: 'Settings', icon: 'settings' }
	];
	function active(href: string) {
		const p = page.url.pathname;
		return href === '/' ? p === '/' : p === href || p.startsWith(href + '/');
	}
	const themeIcon = $derived(theme.value === 'light' ? 'sun' : theme.value === 'dark' ? 'moon' : 'auto');
	const themeLabel = $derived(
		theme.value === 'light' ? 'Light theme' : theme.value === 'dark' ? 'Dark theme' : 'Theme follows the system'
	);
</script>

<svelte:window onclick={() => (menuOpen = false)} />

{#if fatal}
	<main class="center">
		<div class="card stack" style="max-width: 420px">
			<h2>Hearth isn't answering</h2>
			<p class="muted">{fatal}</p>
			<button class="primary" onclick={() => location.reload()}>Try again</button>
		</div>
	</main>
{:else if !ready}
	<main class="center"><div class="spinner" aria-label="Loading"></div></main>
{:else if isPublic || !session.me}
	{@render children()}
{:else}
	<div class="shell">
		<aside class="sidebar">
			<a class="brand" href="/">
				<img src="/favicon.svg" alt="" width="30" height="30" />
				<span>Hearth</span>
			</a>
			<nav aria-label="Main">
				{#each nav as n (n.href)}
					<a href={n.href} class:active={active(n.href)} aria-current={active(n.href) ? 'page' : undefined}>
						<Icon name={n.icon} />
						<span>{n.label}</span>
					</a>
				{/each}
			</nav>
			<div class="spacer"></div>
			<div class="host muted small" title="This server">{session.hostname}</div>
		</aside>

		<div class="main-col">
			<header class="topbar">
				<a class="brand mobile-only" href="/">
					<img src="/favicon.svg" alt="" width="26" height="26" />
					<span>{session.hostname || 'Hearth'}</span>
				</a>
				<div class="spacer"></div>
				<button class="ghost" title={themeLabel} aria-label={themeLabel} onclick={() => theme.cycle()}>
					<Icon name={themeIcon} />
				</button>
				<div class="menu-wrap">
					<button
						class="ghost user"
						aria-haspopup="menu"
						aria-expanded={menuOpen}
						onclick={(e) => {
							e.stopPropagation();
							menuOpen = !menuOpen;
						}}
					>
						<Icon name="user" />
						<span class="desktop-only">{session.me.user.username}</span>
					</button>
					{#if menuOpen}
						<div class="menu card" role="menu">
							<div class="who">
								<strong>{session.me.user.username}</strong>
								<span class="badge {session.isAdmin ? 'accent' : ''}">{session.isAdmin ? 'Admin' : 'Read-only'}</span>
							</div>
							<a role="menuitem" href="/settings#account" onclick={() => (menuOpen = false)}><Icon name="key" size={18} /> Change password</a>
							<button role="menuitem" class="ghost" onclick={signOut}><Icon name="logout" size={18} /> Sign out</button>
						</div>
					{/if}
				</div>
			</header>
			<main class="content">
				{@render children()}
			</main>
		</div>

		<nav class="bottombar" aria-label="Main">
			{#each nav as n (n.href)}
				<a href={n.href} class:active={active(n.href)} aria-current={active(n.href) ? 'page' : undefined}>
					<Icon name={n.icon} size={22} />
					<span>{n.label}</span>
				</a>
			{/each}
		</nav>
	</div>
{/if}

<style>
	.center {
		min-height: 100dvh;
		display: grid;
		place-items: center;
		padding: 1rem;
	}
	.spinner {
		width: 34px;
		height: 34px;
		border-radius: 50%;
		border: 3px solid var(--border);
		border-top-color: var(--accent);
		animation: spin 0.8s linear infinite;
	}
	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}
	.shell {
		display: grid;
		grid-template-columns: 220px 1fr;
		min-height: 100dvh;
	}
	.sidebar {
		position: sticky;
		top: 0;
		height: 100dvh;
		display: flex;
		flex-direction: column;
		gap: 1rem;
		padding: 1rem 0.75rem;
		border-right: 1px solid var(--border);
		background: var(--bg-raised);
	}
	.brand {
		display: flex;
		align-items: center;
		gap: 0.55rem;
		font-weight: 750;
		font-size: 1.1rem;
		color: var(--text);
		text-decoration: none;
		padding: 0.2rem 0.5rem;
	}
	.sidebar nav {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
	}
	.sidebar nav a {
		display: flex;
		align-items: center;
		gap: 0.7rem;
		padding: 0.6rem 0.75rem;
		border-radius: var(--radius-sm);
		color: var(--text-muted);
		text-decoration: none;
		font-weight: 600;
	}
	.sidebar nav a:hover {
		background: var(--bg-sunken);
		color: var(--text);
	}
	.sidebar nav a.active {
		background: var(--accent-soft);
		color: var(--accent-strong);
	}
	.host {
		padding: 0 0.75rem;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.main-col {
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.topbar {
		position: sticky;
		top: 0;
		z-index: 5;
		display: flex;
		align-items: center;
		gap: 0.25rem;
		padding: 0.5rem 1rem;
		padding-top: max(0.5rem, env(safe-area-inset-top));
		background: color-mix(in srgb, var(--bg) 85%, transparent);
		backdrop-filter: blur(10px);
		border-bottom: 1px solid var(--border);
	}
	.topbar button {
		padding: 0.45rem 0.55rem;
	}
	.content {
		padding: 1.25rem;
		max-width: 1200px;
		width: 100%;
		margin: 0 auto;
	}
	.menu-wrap {
		position: relative;
	}
	.menu {
		position: absolute;
		right: 0;
		top: calc(100% + 6px);
		min-width: 220px;
		padding: 0.4rem;
		display: flex;
		flex-direction: column;
		z-index: 20;
	}
	.menu .who {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		padding: 0.5rem 0.6rem 0.6rem;
		border-bottom: 1px solid var(--border);
		margin-bottom: 0.3rem;
	}
	.menu a,
	.menu button {
		display: flex;
		align-items: center;
		justify-content: flex-start;
		gap: 0.6rem;
		padding: 0.55rem 0.6rem;
		border-radius: var(--radius-sm);
		color: var(--text);
		text-decoration: none;
		font-weight: 500;
		width: 100%;
	}
	.menu a:hover {
		background: var(--bg-sunken);
	}
	.bottombar,
	.mobile-only {
		display: none;
	}
	@media (max-width: 760px) {
		.shell {
			grid-template-columns: 1fr;
		}
		.sidebar,
		.desktop-only {
			display: none;
		}
		.mobile-only {
			display: flex;
		}
		.content {
			padding: 1rem 0.85rem calc(5.5rem + env(safe-area-inset-bottom));
		}
		.bottombar {
			display: grid;
			grid-template-columns: repeat(4, 1fr);
			position: fixed;
			left: 0;
			right: 0;
			bottom: 0;
			z-index: 10;
			background: var(--bg-raised);
			border-top: 1px solid var(--border);
			padding-bottom: env(safe-area-inset-bottom);
		}
		.bottombar a {
			display: flex;
			flex-direction: column;
			align-items: center;
			gap: 0.15rem;
			padding: 0.5rem 0 0.45rem;
			font-size: 0.72rem;
			font-weight: 600;
			color: var(--text-muted);
			text-decoration: none;
		}
		.bottombar a.active {
			color: var(--accent-strong);
		}
	}
</style>
