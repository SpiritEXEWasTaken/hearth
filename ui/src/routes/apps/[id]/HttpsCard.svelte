<script lang="ts">
	import { onMount } from 'svelte';
	import { get, put, post, errorText } from '#lib/api.ts';
	import type { AppDetail, HttpsView, HttpsTest } from '#lib/types.ts';
	import type { JobRun } from '#lib/components/jobrun.svelte.ts';
	import Icon from '#lib/components/Icon.svelte';
	import Modal from '#lib/components/Modal.svelte';

	let {
		app,
		canChange,
		job,
		onchange
	}: { app: AppDetail; canChange: boolean; job: JobRun; onchange: () => void } = $props();

	let v = $state<HttpsView | null>(null);
	let error = $state('');
	let port = $state<number | null>(null);
	let subdomain = $state('');
	let busy = $state(false);
	let test = $state<HttpsTest | null>(null);
	let testing = $state(false);
	let fixInfo = $state<{ file: string; action: string; lines: string; reason?: string } | null>(null);
	let fixOpen = $state(false);

	const base = $derived(`/v1/apps/${encodeURIComponent(app.id)}/https`);

	async function load() {
		try {
			v = await get<HttpsView>(base);
			port = v.port ?? v.suggested_port;
			subdomain = v.subdomain ?? v.suggested_subdomain ?? '';
			error = '';
		} catch (e) {
			error = errorText(e);
		}
	}
	onMount(load);

	async function setEnabled(enabled: boolean) {
		busy = true;
		error = '';
		test = null;
		try {
			v = await put<HttpsView>(base, {
				enabled,
				port: enabled ? port : undefined,
				subdomain: enabled && v?.domain ? subdomain : undefined
			});
			onchange();
			if (enabled) await runTest();
		} catch (e) {
			error = errorText(e);
		} finally {
			busy = false;
		}
	}

	async function saveSubdomain() {
		busy = true;
		error = '';
		try {
			v = await put<HttpsView>(base, { enabled: true, subdomain });
			onchange();
		} catch (e) {
			error = errorText(e);
		} finally {
			busy = false;
		}
	}

	async function runTest() {
		testing = true;
		try {
			test = await post<HttpsTest>(`${base}/test`, {});
		} catch (e) {
			error = errorText(e);
		} finally {
			testing = false;
		}
	}

	async function reviewFix() {
		error = '';
		try {
			fixInfo = await post(`/v1/apps/${encodeURIComponent(app.id)}/fix/ha-proxy`, { dry_run: true });
			fixOpen = true;
		} catch (e) {
			error = errorText(e);
		}
	}

	async function applyFix() {
		fixOpen = false;
		const done = await job.run(`Setting up ${app.title} for HTTPS`, () =>
			post(`/v1/apps/${encodeURIComponent(app.id)}/fix/ha-proxy`, {})
		);
		if (done?.status === 'succeeded') {
			// HA takes a little while to start answering again.
			await new Promise((r) => setTimeout(r, 5000));
			await runTest();
		}
		onchange();
	}

	// The address matching how this browser reached Hearth, if there is one.
	const best = $derived(v?.urls.find((u) => new URL(u).hostname === location.hostname) ?? v?.urls[0]);
</script>

<div class="card stack">
	<div class="row">
		<h2><Icon name="lock" size={18} /> HTTPS</h2>
		{#if v?.enabled}<span class="badge {v.running ? 'ok' : 'bad'}">{v.running ? 'On' : 'Not listening'}</span>{/if}
	</div>
	{#if error}<div class="notice bad">{error}</div>{/if}
	{#if !v}
		<span class="muted">Loading…</span>
	{:else if v.unavailable && !v.enabled}
		<p class="muted small">Not available: {v.unavailable}.</p>
	{:else if !v.enabled}
		<p class="muted small">
			Open {app.title} with a secure https:// address, using the same certificate as Hearth. Hearth
			forwards it to the app's own port ({v.target_port}). Some apps (password managers, cameras, streaming) only
			work fully over HTTPS.
		</p>
		{#if canChange}
			<div class="row">
				<label class="inline">Port <input type="number" min="1024" max="65535" bind:value={port} /></label>
				{#if v.domain}
					<label class="inline">Name <input class="sub" bind:value={subdomain} aria-label="Subdomain" />.{v.domain}</label>
				{/if}
				<button class="primary" onclick={() => setEnabled(true)} disabled={busy || !port}>
					{busy ? 'Turning on…' : 'Use HTTPS'}
				</button>
			</div>
		{/if}
	{:else}
		<div class="urls">
			{#if best}<a class="btn primary" href={best} target="_blank" rel="noopener noreferrer"><Icon name="external" size={16} /> Open securely</a>{/if}
			{#each v.urls as u (u)}<a class="mono small" href={u} target="_blank" rel="noopener noreferrer">{u}</a>{/each}
		</div>
		<div class="muted small">Forwards to the app's port {v.target_port}.</div>
		{#if v.domain && canChange}
			<div class="row small">
				<label class="inline">Name <input class="sub" bind:value={subdomain} aria-label="Subdomain" placeholder="none" />.{v.domain}</label>
				{#if subdomain !== (v.subdomain ?? '')}<button class="small" onclick={saveSubdomain} disabled={busy}>Save</button>{/if}
			</div>
		{/if}
		{#if test}
			<div class="notice {test.ok ? 'ok' : 'warn'}" role="status">
				{test.message}
				{#if test.fix === 'ha-proxy' && canChange}
					<div class="fixrow">
						<button class="primary small" onclick={reviewFix} disabled={job.active}>Fix it</button>
						<span class="small">Adds a few lines to configuration.yaml (a backup is kept) and restarts Home Assistant.</span>
					</div>
				{/if}
			</div>
		{/if}
		<div class="row">
			<button class="small" onclick={runTest} disabled={testing}>{testing ? 'Testing…' : 'Test'}</button>
			{#if canChange}<button class="small" onclick={() => setEnabled(false)} disabled={busy}>Turn off</button>{/if}
		</div>
	{/if}
</div>

<Modal title="Let Home Assistant work through HTTPS?" bind:open={fixOpen}>
	{#if fixInfo}
		{#if fixInfo.action === 'add'}
			<p>Hearth adds these lines to <span class="mono">{fixInfo.file}</span>, keeps the old file next to it, and restarts Home Assistant (about a minute):</p>
			<pre>{fixInfo.lines}</pre>
		{:else if fixInfo.action === 'add-storage'}
			<p>
				Home Assistant keeps its network settings in <span class="mono">{fixInfo.file}</span>. Hearth stops Home Assistant,
				adds this there (keeping a backup next to it), and starts it again (about a minute):
			</p>
			<pre>{fixInfo.lines}</pre>
		{:else if fixInfo.action === 'present'}
			<p>Home Assistant already trusts a proxy. If it still refuses, check the addresses under <span class="mono">trusted_proxies</span> in {fixInfo.file}.</p>
		{:else if fixInfo.reason}
			<p>{fixInfo.reason}</p>
		{:else}
			<p>{fixInfo.file} already has an <span class="mono">http:</span> section, so Hearth won't edit it. Add these lines under it yourself, then restart Home Assistant:</p>
			<pre>{fixInfo.lines.replace(/^http:\n/, '')}</pre>
		{/if}
	{/if}
	{#snippet actions()}
		{@const canApply = fixInfo?.action === 'add' || fixInfo?.action === 'add-storage'}
		<button onclick={() => (fixOpen = false)}>{canApply ? 'Cancel' : 'Close'}</button>
		{#if canApply}<button class="primary" onclick={applyFix}>Change and restart</button>{/if}
	{/snippet}
</Modal>

<style>
	h2 {
		display: flex;
		align-items: center;
		gap: 0.45rem;
	}
	.urls {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.35rem;
	}
	.urls .btn {
		margin-bottom: 0.2rem;
	}
	.inline {
		flex-direction: row;
		align-items: center;
		gap: 0.5rem;
	}
	.inline input {
		width: 7rem;
	}
	.inline input.sub {
		width: 9rem;
	}
	.fixrow {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		margin-top: 0.5rem;
		flex-wrap: wrap;
	}
</style>
