<script lang="ts">
	import { goto } from '$app/navigation';
	import { post, errorText } from '#lib/api.ts';
	import { session } from '#lib/session.svelte.ts';
	import type { Me } from '#lib/types.ts';

	let code = $state('');
	let username = $state('');
	let password = $state('');
	let password2 = $state('');
	let error = $state('');
	let busy = $state(false);

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		error = '';
		if (password !== password2) {
			error = "The two passwords don't match.";
			return;
		}
		busy = true;
		try {
			session.set(await post<Me>('/v1/setup', { code, username, password }));
			await goto('/', { replace: true });
		} catch (err) {
			error = errorText(err);
		} finally {
			busy = false;
		}
	}
</script>

<svelte:head><title>Set up Hearth</title></svelte:head>

<main class="auth">
	<form class="card stack" onsubmit={submit}>
		<div class="head">
			<img src="/favicon.svg" alt="" width="44" height="44" />
			<div>
				<h1>Welcome to Hearth</h1>
				<p class="muted small">Create the admin account for {session.hostname || 'this server'}.</p>
			</div>
		</div>
		<label>
			Setup code
			<span class="hint">
				To prove you own this server. On the server, run <code class="mono">hearth web setup-code</code>.
			</span>
			<input
				name="code"
				bind:value={code}
				autocomplete="one-time-code"
				autocapitalize="characters"
				spellcheck="false"
				placeholder="ABCD-EFGH"
				required
			/>
		</label>
		<label>
			Username
			<input name="username" bind:value={username} autocomplete="username" autocapitalize="none" spellcheck="false" required />
		</label>
		<label>
			Password
			<span class="hint">At least 8 characters.</span>
			<input name="password" type="password" bind:value={password} autocomplete="new-password" minlength="8" required />
		</label>
		<label>
			Password again
			<input name="password2" type="password" bind:value={password2} autocomplete="new-password" minlength="8" required />
		</label>
		{#if error}<div class="notice bad" role="alert">{error}</div>{/if}
		<button class="primary" type="submit" disabled={busy}>{busy ? 'Creating…' : 'Create admin account'}</button>
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
		width: min(420px, 100%);
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
