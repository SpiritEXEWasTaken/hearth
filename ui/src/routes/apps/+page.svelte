<script lang="ts">
	import { onMount } from 'svelte';
	import { get, post, errorText } from '#lib/api.ts';
	import type { App } from '#lib/types.ts';
	import { bytes, updateCount, stateLabel, kindLabel } from '#lib/format.ts';
	import AppIcon from '#lib/components/AppIcon.svelte';
	import Icon from '#lib/components/Icon.svelte';

	let apps = $state<App[] | null>(null);
	let error = $state('');
	let query = $state('');
	let filter = $state<'all' | 'web' | 'updates' | 'problems'>('all');
	let checking = $state(false);

	async function load() {
		try {
			apps = await get<App[]>('/v1/apps');
			error = '';
		} catch (e) {
			error = errorText(e);
		}
	}

	async function checkNow() {
		checking = true;
		try {
			await post('/v1/versions/check', {});
			await load();
		} catch (e) {
			error = errorText(e);
		} finally {
			checking = false;
		}
	}

	onMount(() => {
		load();
		const t = setInterval(() => document.visibilityState === 'visible' && load(), 10000);
		return () => clearInterval(t);
	});

	const shown = $derived(
		(apps ?? [])
			.filter((a) => {
				const q = query.trim().toLowerCase();
				if (q && !a.title.toLowerCase().includes(q) && !a.id.toLowerCase().includes(q)) return false;
				if (filter === 'web') return a.dashboard;
				if (filter === 'updates') return updateCount(a) > 0;
				if (filter === 'problems') return a.state === 'unhealthy' || a.state === 'partial' || a.health === 'unhealthy';
				return true;
			})
			.sort((a, b) => Number(b.dashboard) - Number(a.dashboard) || a.title.localeCompare(b.title))
	);

	function version(a: App): string {
		const v = a.images.map((i) => i.running_version).filter(Boolean);
		return v.length ? (v[0] as string) : '';
	}
</script>

<svelte:head><title>Apps · Hearth</title></svelte:head>

<div class="head">
	<h1>Apps</h1>
	<div class="spacer"></div>
	<button onclick={checkNow} disabled={checking} title="Ask the registries for new versions now">
		<Icon name="update" size={18} />
		{checking ? 'Checking…' : 'Check for updates'}
	</button>
</div>

<div class="tools">
	<div class="search">
		<Icon name="search" size={18} />
		<input type="search" placeholder="Search apps" bind:value={query} aria-label="Search apps" />
	</div>
	<div class="chips" role="group" aria-label="Filter">
		{#each [['all', 'All'], ['web', 'With a web page'], ['updates', 'Updates'], ['problems', 'Problems']] as [k, label] (k)}
			<button class="small" class:on={filter === k} aria-pressed={filter === k} onclick={() => (filter = k as typeof filter)}>
				{label}
			</button>
		{/each}
	</div>
</div>

{#if error}<div class="notice bad">{error}</div>{/if}

{#if apps === null}
	<div class="card muted">Loading…</div>
{:else if shown.length === 0}
	<div class="card muted">No apps match.</div>
{:else}
	<div class="list card">
		{#each shown as app (app.id)}
			{@const st = stateLabel(app)}
			{@const n = updateCount(app)}
			<a class="item" href={`/apps/${encodeURIComponent(app.id)}`}>
				<AppIcon {app} size={40} />
				<span class="main">
					<span class="title">{app.title}</span>
					<span class="meta muted small">
						{kindLabel(app)}
						{#if version(app)}· {version(app)}{/if}
						{#if app.memory_bytes}· {bytes(app.memory_bytes)}{/if}
						{#if app.read_only}· <span title="Hearth only watches this app">view only</span>{/if}
					</span>
				</span>
				<span class="badges">
					{#if n > 0}<span class="badge accent">Update</span>{/if}
					<span class="badge {st.tone}">{st.text}</span>
				</span>
				<Icon name="chevron" size={16} />
			</a>
		{/each}
	</div>
{/if}

<style>
	.head {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		margin-bottom: 1rem;
	}
	.tools {
		display: flex;
		gap: 0.75rem;
		flex-wrap: wrap;
		align-items: center;
		margin-bottom: 1rem;
	}
	.search {
		position: relative;
		flex: 1 1 240px;
		color: var(--text-muted);
	}
	.search :global(svg) {
		position: absolute;
		left: 0.7rem;
		top: 50%;
		transform: translateY(-50%);
	}
	.search input {
		padding-left: 2.3rem;
	}
	.chips {
		display: flex;
		gap: 0.4rem;
		flex-wrap: wrap;
	}
	.chips button.on {
		background: var(--accent-soft);
		border-color: var(--accent);
		color: var(--accent-strong);
	}
	.list {
		padding: 0.3rem;
	}
	.item {
		display: flex;
		align-items: center;
		gap: 0.8rem;
		padding: 0.65rem;
		border-radius: var(--radius-sm);
		color: var(--text);
		text-decoration: none;
	}
	.item + .item {
		border-top: 1px solid var(--border);
	}
	.item:hover {
		background: var(--bg-sunken);
	}
	.main {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.title {
		font-weight: 650;
	}
	.meta {
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.badges {
		display: flex;
		gap: 0.35rem;
		flex-wrap: wrap;
		justify-content: flex-end;
	}
</style>
