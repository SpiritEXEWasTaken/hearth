<script lang="ts">
	import { onMount } from 'svelte';
	import { get, errorText } from '#lib/api.ts';
	import type { AppDetail } from '#lib/types.ts';
	import Icon from '#lib/components/Icon.svelte';

	let { app }: { app: AppDetail } = $props();
	type Line = { service: string; stream: string; line: string };
	let lines = $state<Line[]>([]);
	let error = $state('');
	let service = $state('');
	let tail = $state(300);
	let search = $state('');
	let follow = $state(true);
	let box: HTMLDivElement | undefined = $state();
	let atBottom = true;

	const services = $derived([...new Set(app.containers.map((c) => c.service ?? c.name))]);

	async function load() {
		try {
			const q = new URLSearchParams({ tail: String(tail) });
			if (service) q.set('service', service);
			lines = await get<Line[]>(`/v1/apps/${encodeURIComponent(app.id)}/logs?${q}`);
			error = '';
			if (box && atBottom) requestAnimationFrame(() => box && (box.scrollTop = box.scrollHeight));
		} catch (e) {
			error = errorText(e);
		}
	}

	onMount(() => {
		load();
		const t = setInterval(() => follow && document.visibilityState === 'visible' && load(), 3000);
		return () => clearInterval(t);
	});

	function onScroll() {
		if (box) atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 40;
	}

	const shown = $derived.by(() => {
		const q = search.trim().toLowerCase();
		return q ? lines.filter((l) => l.line.toLowerCase().includes(q)) : lines;
	});
	const multi = $derived(services.length > 1);
</script>

<div class="stack">
	<div class="tools">
		{#if multi}
			<select bind:value={service} onchange={load} aria-label="Part of the app">
				<option value="">All parts</option>
				{#each services as s (s)}<option value={s}>{s}</option>{/each}
			</select>
		{/if}
		<select bind:value={tail} onchange={load} aria-label="How many lines">
			<option value={100}>Last 100 lines</option>
			<option value={300}>Last 300 lines</option>
			<option value={1000}>Last 1000 lines</option>
			<option value={5000}>Last 5000 lines</option>
		</select>
		<div class="search">
			<Icon name="search" size={16} />
			<input type="search" placeholder="Search the log" bind:value={search} aria-label="Search the log" />
		</div>
		<label class="follow"><input type="checkbox" bind:checked={follow} /> Live</label>
	</div>
	{#if error}<div class="notice bad">{error}</div>{/if}
	<div class="log" bind:this={box} onscroll={onScroll} role="log" aria-live="off">
		{#if shown.length === 0}
			<div class="muted">{lines.length ? 'Nothing matches.' : 'No log lines.'}</div>
		{/if}
		{#each shown as l, i (i)}
			<div class="line" class:err={l.stream === 'stderr'}>
				{#if multi && !service}<span class="svc">{l.service}</span>{/if}{l.line}
			</div>
		{/each}
	</div>
</div>

<style>
	.tools {
		display: flex;
		gap: 0.6rem;
		flex-wrap: wrap;
		align-items: center;
	}
	.tools select {
		width: auto;
	}
	.search {
		position: relative;
		flex: 1 1 200px;
		color: var(--text-muted);
	}
	.search :global(svg) {
		position: absolute;
		left: 0.65rem;
		top: 50%;
		transform: translateY(-50%);
	}
	.search input {
		padding-left: 2.1rem;
	}
	.follow {
		flex-direction: row;
		align-items: center;
		gap: 0.4rem;
		font-weight: 500;
	}
	.follow input {
		width: auto;
		min-height: 0;
	}
	.log {
		background: #0f0d0c;
		color: #e7e2dc;
		border-radius: var(--radius-sm);
		padding: 0.7rem 0.85rem;
		font-family: var(--mono);
		font-size: 0.78rem;
		line-height: 1.5;
		height: min(65vh, 640px);
		overflow: auto;
	}
	.line {
		white-space: pre-wrap;
		word-break: break-word;
	}
	.line.err {
		color: #fca5a5;
	}
	.svc {
		color: #fdba74;
		margin-right: 0.6rem;
	}
</style>
