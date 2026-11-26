<script lang="ts">
	import { onMount } from 'svelte';
	import { get, post, errorText } from '#lib/api.ts';
	import type { StoreList } from '#lib/types.ts';
	import { badges } from '#lib/store.ts';
	import { session } from '#lib/session.svelte.ts';
	import Icon from '#lib/components/Icon.svelte';

	let list = $state<StoreList | null>(null);
	let error = $state('');
	let query = $state('');
	let category = $state('');
	let refreshing = $state(false);

	async function load() {
		try {
			list = await get<StoreList>('/v1/store');
			error = '';
		} catch (e) {
			error = errorText(e);
		}
	}

	async function refresh() {
		refreshing = true;
		try {
			await post('/v1/store/refresh', {});
			await load();
		} catch (e) {
			error = errorText(e);
		} finally {
			refreshing = false;
		}
	}

	onMount(() => {
		const params = new URLSearchParams(location.search);
		query = params.get('q') ?? '';
		category = params.get('category') ?? '';
		load();
	});

	const shown = $derived(
		(list?.apps ?? []).filter((a) => {
			if (category && a.category !== category) return false;
			const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
			const text = `${a.name} ${a.summary} ${(a.tags ?? []).join(' ')} ${a.category}`.toLowerCase();
			return words.every((w) => text.includes(w));
		})
	);

	function when(t: number | null) {
		return t ? new Date(t * 1000).toLocaleString() : 'never';
	}
</script>

<svelte:head><title>Store · Hearth</title></svelte:head>

<div class="head">
	<h1>Store</h1>
	<div class="spacer"></div>
	{#if session.isAdmin}
		<button onclick={refresh} disabled={refreshing} title="Download the latest app list now">
			<Icon name="restart" size={18} />
			{refreshing ? 'Refreshing…' : 'Refresh'}
		</button>
	{/if}
</div>

<div class="tools">
	<div class="search">
		<Icon name="search" size={18} />
		<input type="search" placeholder="Search the store" bind:value={query} aria-label="Search the store" />
	</div>
</div>
{#if list && list.categories.length}
	<div class="chips" role="group" aria-label="Categories">
		<button class="small" class:on={category === ''} aria-pressed={category === ''} onclick={() => (category = '')}>All</button>
		{#each list.categories as c (c.id)}
			<button class="small" class:on={category === c.id} aria-pressed={category === c.id} onclick={() => (category = c.id)}>
				{c.name}
			</button>
		{/each}
	</div>
{/if}

{#if error}<div class="notice bad">{error}</div>{/if}
{#if list?.error}
	<div class="notice warn">
		{list.apps.length ? 'The last store update failed: ' : ''}{list.error}
		{#if list.apps.length}(showing the list from {when(list.generated)}){/if}
	</div>
{/if}

{#if list === null}
	<div class="card muted">Loading…</div>
{:else if list.apps.length && shown.length === 0}
	<div class="card muted">No apps match.</div>
{:else}
	<div class="grid">
		{#each shown as a (a.id)}
			<a class="card app" href={`/store/${encodeURIComponent(a.id)}`} data-testid="store-card">
				<img src={`/v1/store/icons/${encodeURIComponent(a.id)}`} alt="" width="52" height="52" loading="lazy" />
				<span class="main">
					<span class="title">
						{a.name}
						{#if a.installed.length}<span class="badge ok" title={`Installed as ${a.installed.join(', ')}`}>Installed</span>{/if}
					</span>
					<span class="muted small summary">{a.summary}</span>
					<span class="badges">
						{#each badges(a.badges, list.host) as b (b.kind)}
							<span class="badge {b.tone ?? ''}" title={b.title}>{b.text}</span>
						{/each}
					</span>
				</span>
			</a>
		{/each}
	</div>
	{#if list.generated}
		<p class="muted small foot">
			{list.apps.length} apps · list from {when(list.generated)} · signed and checked by Hearth
		</p>
	{/if}
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
		margin-bottom: 0.75rem;
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
		margin-bottom: 1rem;
	}
	.chips button.on {
		background: var(--accent-soft);
		border-color: var(--accent);
		color: var(--accent-strong);
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
		gap: 0.85rem;
	}
	.app {
		display: flex;
		gap: 0.85rem;
		align-items: flex-start;
		color: var(--text);
		text-decoration: none;
	}
	.app:hover {
		border-color: var(--accent);
	}
	.app img {
		object-fit: contain;
		flex: none;
		border-radius: 22%;
	}
	.main {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		min-width: 0;
	}
	.title {
		font-weight: 650;
		display: flex;
		gap: 0.4rem;
		align-items: center;
		flex-wrap: wrap;
	}
	.badges {
		display: flex;
		gap: 0.3rem;
		flex-wrap: wrap;
	}
	.badges .badge {
		font-size: 0.75rem;
	}
	.foot {
		margin-top: 1rem;
	}
</style>
