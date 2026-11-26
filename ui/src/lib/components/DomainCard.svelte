<script lang="ts">
	import { onMount } from 'svelte';
	import { get, put, del, post, errorText } from '#lib/api.ts';
	import { session } from '#lib/session.svelte.ts';
	import type { DomainStatus } from '#lib/types.ts';
	import { dateTime, ago } from '#lib/format.ts';
	import Modal from '#lib/components/Modal.svelte';
	import JobPanel from '#lib/components/JobPanel.svelte';
	import { JobRun } from '#lib/components/jobrun.svelte.ts';

	let d = $state<DomainStatus | null>(null);
	let name = $state('');
	let token = $state('');
	let address = $state('tailscale');
	let error = $state('');
	let editing = $state(false);
	const job = new JobRun();

	async function load() {
		try {
			d = await get<DomainStatus>('/v1/domain');
			name = d.name ?? '';
			address = d.address ?? 'tailscale';
		} catch (e) {
			error = errorText(e);
		}
	}
	onMount(load);

	async function save(e: SubmitEvent) {
		e.preventDefault();
		error = '';
		const t = token;
		await job.run('Setting up your domain', () => put('/v1/domain', { name, token: t || undefined, address }));
		token = '';
		editing = false;
		await load();
	}

	async function renew() {
		await job.run('Renewing the certificates', () => post('/v1/domain/renew', {}));
		await load();
	}

	let removeOpen = $state(false);
	async function remove() {
		removeOpen = false;
		try {
			await del('/v1/domain');
		} catch (e) {
			error = errorText(e);
		}
		await load();
	}
</script>

<div class="stack">
	<h3>Your own domain (DuckDNS)</h3>
	{#if error}<div class="notice bad">{error}</div>{/if}
	{#if d?.configured && !editing}
		<div>
			<a href="https://{d.base}" class="mono">https://{d.base}</a>
			<div class="muted small">
				Points to {d.ip ?? '…'} ({d.address === 'tailscale' ? 'Tailscale' : 'home network'}). Apps get
				<span class="mono">https://&lt;name&gt;.{d.base}</span> in their HTTPS card.
			</div>
		</div>
		{#each d.certificates as c (c.name)}
			<div class="small"><span class="badge ok">Certificate</span> <span class="mono">{c.name}</span> · valid until {dateTime(c.expires_at)}</div>
		{/each}
		{#if d.last_error}
			<div class="notice bad">Last attempt failed: {d.last_error}</div>
		{:else if d.last_ok}
			<div class="muted small">Renewed automatically a month before they expire. Last success {ago(d.last_ok)}.</div>
		{/if}
		{#if session.isAdmin}
			<div class="row">
				<button class="small" onclick={renew} disabled={job.active}>Renew now</button>
				<button class="small" onclick={() => (editing = true)}>Change</button>
				<button class="small danger" onclick={() => (removeOpen = true)}>Remove</button>
			</div>
		{/if}
	{:else if session.isAdmin}
		<p class="muted small">
			Give Hearth and your apps names like <span class="mono">ha.yourname.duckdns.org</span> with real Let's Encrypt
			certificates. No ports are opened: the name only points to this server's {address === 'tailscale'
				? 'Tailscale'
				: 'home network'} address, and the certificate is proven through DuckDNS.
		</p>
		<form class="stack" onsubmit={save}>
			<label>
				DuckDNS name
				<span class="hint">The part before .duckdns.org.</span>
				<span class="nameinput"><input bind:value={name} autocapitalize="none" spellcheck="false" required placeholder="yourname" /><span class="muted">.duckdns.org</span></span>
			</label>
			<label>
				DuckDNS token
				<span class="hint">Shown at the top of duckdns.org after you sign in. Stored only on this server, never shown again.</span>
				<input type="password" bind:value={token} autocomplete="off" required={!d?.configured} placeholder={d?.configured ? 'unchanged' : ''} />
			</label>
			<label>
				The name points to
				<select bind:value={address}>
					<option value="tailscale">This server's Tailscale address (works anywhere with Tailscale)</option>
					<option value="lan">This server's home network address (works at home without Tailscale)</option>
				</select>
			</label>
			<div class="row">
				<button class="primary" type="submit" disabled={job.active}>Save and get certificates</button>
				{#if editing}<button type="button" onclick={() => (editing = false)}>Cancel</button>{/if}
			</div>
			<div class="muted small">The first time takes about two minutes: Hearth checks everything with Let's Encrypt's test service first.</div>
		</form>
	{:else}
		<p class="muted small">Not set up.</p>
	{/if}
</div>

<Modal title={job.title || 'Working'} bind:open={job.open} wide>
	<JobPanel title="" lines={job.lines} job={job.job} error={job.error} />
	{#snippet actions()}
		<button class="primary" onclick={() => (job.open = false)} disabled={job.active}>{job.active ? 'Working…' : 'Close'}</button>
	{/snippet}
</Modal>

<Modal title="Remove your domain?" bind:open={removeOpen}>
	<p>Hearth forgets the name, the token and its certificates. App addresses with :port keep working.</p>
	{#snippet actions()}
		<button onclick={() => (removeOpen = false)}>Cancel</button>
		<button class="danger solid" onclick={remove}>Remove</button>
	{/snippet}
</Modal>

<style>
	.nameinput {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		font-weight: 400;
	}
	.nameinput input {
		flex: 1;
	}
	h3 {
		margin-top: 0.4rem;
	}
</style>
