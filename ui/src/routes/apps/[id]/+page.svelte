<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { get, post, errorText } from '#lib/api.ts';
	import { session } from '#lib/session.svelte.ts';
	import type { AppDetail } from '#lib/types.ts';
	import { bytes, openUrl, stateLabel, kindLabel, updateCount } from '#lib/format.ts';
	import AppIcon from '#lib/components/AppIcon.svelte';
	import Icon from '#lib/components/Icon.svelte';
	import Modal from '#lib/components/Modal.svelte';
	import JobPanel from '#lib/components/JobPanel.svelte';
	import { JobRun } from '#lib/components/jobrun.svelte.ts';
	import Overview from './Overview.svelte';
	import Logs from './Logs.svelte';
	import Settings from './Settings.svelte';
	import Updates from './Updates.svelte';
	import Backups from './Backups.svelte';
	import History from './History.svelte';

	const id = $derived(page.params.id ?? '');
	let app = $state<AppDetail | null>(null);
	let error = $state('');
	const job = new JobRun();

	async function load() {
		try {
			app = await get<AppDetail>(`/v1/apps/${encodeURIComponent(id)}`);
			error = '';
		} catch (e) {
			error = errorText(e);
		}
	}

	onMount(() => {
		load();
		const t = setInterval(() => document.visibilityState === 'visible' && !job.active && load(), 5000);
		return () => clearInterval(t);
	});

	const managed = $derived(!!app && app.managed_by === 'hearth' && app.kind === 'compose');
	const canChange = $derived(!!app && session.isAdmin && !app.read_only);
	const tabs = $derived.by(() => {
		const t: [string, string, string][] = [
			['overview', 'Overview', 'info'],
			['logs', 'Logs', 'logs']
		];
		if (!app) return t;
		if (app.kind === 'compose') t.push(['settings', managed ? 'Settings' : 'Compose file', 'edit']);
		if (managed) {
			t.push(['updates', 'Updates', 'update'], ['backups', 'Backups', 'backup'], ['history', 'History', 'history']);
		}
		return t;
	});
	const tab = $derived(page.url.searchParams.get('tab') ?? 'overview');

	function setTab(t: string) {
		const q = new URLSearchParams(page.url.search);
		if (t === 'overview') q.delete('tab');
		else q.set('tab', t);
		const search = q.toString();
		goto(page.url.pathname + (search ? `?${search}` : ''), { replace: true, reset: false });
	}

	async function action(kind: 'start' | 'stop' | 'restart') {
		if (!app) return;
		const label = { start: 'Starting', stop: 'Stopping', restart: 'Restarting' }[kind];
		await job.run(`${label} ${app.title}`, () => post(`/v1/apps/${encodeURIComponent(id)}/${kind}`));
		await load();
	}

	const url = $derived(app ? openUrl(app, session.hostname) : null);
	const st = $derived(app ? stateLabel(app) : null);
</script>

<svelte:head><title>{app?.title ?? id} · Hearth</title></svelte:head>

<a class="back muted small" href="/apps"><Icon name="back" size={16} /> Apps</a>

{#if error && !app}
	<div class="notice bad">{error}</div>
{:else if !app}
	<div class="card muted">Loading…</div>
{:else}
	<header class="app-head">
		<AppIcon {app} size={64} />
		<div class="title">
			<h1>{app.title}</h1>
			<div class="row small">
				{#if st}<span class="badge {st.tone}">{st.text}</span>{/if}
				<span class="badge">{kindLabel(app)}</span>
				{#if updateCount(app) > 0}<span class="badge accent">Update available</span>{/if}
				{#if app.read_only}<span class="badge info" title="Hearth shows this app but doesn't change it">View only</span>{/if}
				{#if app.managed_by === 'casaos'}<span class="badge warn">Still in CasaOS</span>{/if}
			</div>
		</div>
		<div class="actions">
			{#if url}
				<a class="btn primary" href={url} target="_blank" rel="noopener noreferrer"><Icon name="external" size={18} /> Open</a>
			{/if}
			{#if canChange && app.kind !== 'link'}
				{#if app.state === 'stopped'}
					<button onclick={() => action('start')} disabled={job.active}><Icon name="play" size={16} /> Start</button>
				{:else}
					<button onclick={() => action('restart')} disabled={job.active}><Icon name="restart" size={18} /> Restart</button>
					<button onclick={() => action('stop')} disabled={job.active}><Icon name="stop" size={14} /> Stop</button>
				{/if}
			{/if}
		</div>
	</header>

	<div class="tabs" role="tablist">
		{#each tabs as [key, label, icon] (key)}
			<button role="tab" aria-selected={tab === key} class:on={tab === key} onclick={() => setTab(key)}>
				<Icon name={icon} size={16} />
				{label}
			</button>
		{/each}
	</div>

	<div class="tab-body">
		{#if tab === 'logs'}
			<Logs {app} />
		{:else if tab === 'settings' && app.kind === 'compose'}
			<Settings {app} {canChange} {managed} {job} onchange={load} />
		{:else if tab === 'updates' && managed}
			<Updates {app} {canChange} {job} onchange={load} />
		{:else if tab === 'backups' && managed}
			<Backups {app} {canChange} {job} />
		{:else if tab === 'history' && managed}
			<History {app} {canChange} {job} onchange={load} />
		{:else}
			<Overview {app} />
		{/if}
	</div>
{/if}

<Modal title={job.title || 'Working'} bind:open={job.open} wide>
	<JobPanel title="" lines={job.lines} job={job.job} error={job.error} />
	{#snippet actions()}
		<button class="primary" onclick={() => (job.open = false)} disabled={job.active}>{job.active ? 'Working…' : 'Close'}</button>
	{/snippet}
</Modal>

<style>
	.back {
		display: inline-flex;
		align-items: center;
		gap: 0.2rem;
		text-decoration: none;
		margin-bottom: 0.8rem;
	}
	.app-head {
		display: flex;
		align-items: center;
		gap: 1rem;
		flex-wrap: wrap;
		margin-bottom: 1.1rem;
	}
	.title {
		flex: 1;
		min-width: 200px;
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
	}
	.actions {
		display: flex;
		gap: 0.5rem;
		flex-wrap: wrap;
	}
	.tabs {
		display: flex;
		gap: 0.25rem;
		border-bottom: 1px solid var(--border);
		margin-bottom: 1rem;
		overflow-x: auto;
		scrollbar-width: none;
	}
	.tabs button {
		border: none;
		background: none;
		border-radius: 0;
		border-bottom: 2px solid transparent;
		color: var(--text-muted);
		padding: 0.55rem 0.8rem;
		white-space: nowrap;
	}
	.tabs button.on {
		color: var(--accent-strong);
		border-bottom-color: var(--accent);
	}
	.tabs button:hover:not(.on) {
		color: var(--text);
	}
</style>
