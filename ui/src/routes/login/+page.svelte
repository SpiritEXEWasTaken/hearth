<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { post, errorText } from '#lib/api.ts';
	import { session } from '#lib/session.svelte.ts';
	import type { Me } from '#lib/types.ts';

	let username = $state('');
	let password = $state('');
	let error = $state('');
	let busy = $state(false);

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		error = '';
		busy = true;
		try {
			session.set(await post<Me>('/v1/auth/login', { username, password }));
			const next = page.url.searchParams.get('next');
			await goto(next && next.startsWith('/') && !next.startsWith('//') ? next : '/', { replace: true });
		} catch (err) {
			error = errorText(err);
			password = '';
		} finally {
			busy = false;
		}
	}
</script>

<svelte:head><title>Sign in · Hearth</title></svelte:head>

<main class="auth">
	<form class="card stack" onsubmit={submit}>
		<div class="head">
			<img src="/favicon.svg" alt="" width="44" height="44" />
			<div>
				<h1>Hearth</h1>
				<p class="muted small">Sign in to {session.hostname || 'your server'}</p>
			</div>
		</div>
		<label>
			Username
			<input name="username" bind:value={username} autocomplete="username" autocapitalize="none" spellcheck="false" required />
		</label>
		<label>
			Password
			<input name="password" type="password" bind:value={password} autocomplete="current-password" required />
		</label>
		{#if error}<div class="notice bad" role="alert">{error}</div>{/if}
		<button class="primary" type="submit" disabled={busy}>{busy ? 'Signing in…' : 'Sign in'}</button>
		<p class="muted small">Forgot your password? On the server, run <code class="mono">sudo hearth web user-password NAME</code>.</p>
	</form>
</main>

<style>
	.auth {
		min-height: 100dvh;
		display: grid;
		place-items: center;
		padding: 1rem;
	}
	form {
		width: min(400px, 100%);
		padding: 1.5rem;
	}
	.head {
		display: flex;
		gap: 0.85rem;
		align-items: center;
	}
	code {
		background: var(--bg-sunken);
		padding: 0.05rem 0.3rem;
		border-radius: 5px;
	}
</style>
