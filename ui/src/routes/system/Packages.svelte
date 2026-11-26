<script lang="ts">
	import { get, post, errorText } from '#lib/api.ts';
	import { session } from '#lib/session.svelte.ts';
	import type { PackageEntry, PackageDetail, PackagePlan } from '#lib/types.ts';
	import type { JobRun } from '#lib/components/jobrun.svelte.ts';
	import { bytes } from '#lib/format.ts';
	import Icon from '#lib/components/Icon.svelte';
	import Modal from '#lib/components/Modal.svelte';

	let { job }: { job: JobRun } = $props();
	let q = $state('');
	let backend = $state('all');
	let results = $state<PackageEntry[] | null>(null);
	let searching = $state(false);
	let error = $state('');

	let detail = $state<PackageDetail | null>(null);
	let detailOpen = $state(false);
	let detailError = $state('');

	let plan = $state<PackagePlan | null>(null);
	let planOpen = $state(false);
	let allowCritical = $state(false);

	async function search(e?: SubmitEvent) {
		e?.preventDefault();
		if (!q.trim()) return;
		searching = true;
		error = '';
		try {
			const p = new URLSearchParams({ q: q.trim(), backend });
			// Best matches come first (the server ranks them).
			results = await get<PackageEntry[]>(`/v1/packages/search?${p}`);
		} catch (err) {
			error = errorText(err);
			results = null;
		} finally {
			searching = false;
		}
	}

	function backendParam(p: PackageEntry) {
		return p.backend === 'flatpak' ? 'flatpak' : 'native';
	}

	async function open(p: PackageEntry) {
		detail = { ...p, description: '' };
		detailError = '';
		detailOpen = true;
		try {
			const qs = new URLSearchParams({ name: p.name, backend: backendParam(p) });
			detail = await get<PackageDetail>(`/v1/packages/info?${qs}`);
		} catch (e) {
			detailError = errorText(e);
		}
	}

	let pending: { action: 'install' | 'remove'; name: string; backend: string } | null = null;

	async function review(action: 'install' | 'remove', p: PackageEntry) {
		detailError = '';
		allowCritical = false;
		pending = { action, name: p.name, backend: backendParam(p) };
		try {
			plan = await post<PackagePlan>(`/v1/packages/${action}`, {
				names: [p.name],
				backend: pending.backend,
				dry_run: true
			});
			detailOpen = false;
			planOpen = true;
		} catch (e) {
			detailError = errorText(e);
		}
	}

	async function apply() {
		if (!pending) return;
		const pd = pending;
		planOpen = false;
		await job.run(`${pd.action === 'install' ? 'Installing' : 'Removing'} ${pd.name}`, () =>
			post(`/v1/packages/${pd.action}`, { names: [pd.name], backend: pd.backend, allow_critical: allowCritical })
		);
		if (results) await search();
	}

	const critical = $derived(plan?.errors.some((e) => /critical|needs to stay|allow.critical/i.test(e)) ?? false);
	const blocking = $derived((plan?.errors ?? []).length > 0 && !(critical && allowCritical));
</script>

<div class="stack">
	<form class="row" onsubmit={search}>
		<div class="search">
			<Icon name="search" size={18} />
			<input type="search" placeholder="Search packages and Flatpak apps" bind:value={q} aria-label="Search packages" />
		</div>
		<select bind:value={backend} aria-label="Where to search">
			<option value="all">Everywhere</option>
			<option value="native">System packages</option>
			<option value="flatpak">Flatpak</option>
		</select>
		<button class="primary" type="submit" disabled={searching || !q.trim()}>{searching ? 'Searching…' : 'Search'}</button>
	</form>
	<p class="muted small">
		Install programs from the system's own package manager or Flathub. Hearth shows everything that would change
		before doing it, and won't remove things the server needs to stay reachable without a clear warning.
	</p>
	{#if error}<div class="notice bad">{error}</div>{/if}

	{#if results}
		{#if results.length === 0}
			<div class="card muted">Nothing found.</div>
		{:else}
			<div class="card list">
				{#each results.slice(0, 100) as p (p.backend + p.name + (p.arch ?? ''))}
					<button class="item" onclick={() => open(p)}>
						<span class="grow">
							<span class="name">{p.name}</span>
							<span class="muted small">{p.summary}</span>
						</span>
						<span class="muted small ver">{p.version}</span>
						{#if p.backend === 'flatpak'}<span class="badge info">Flatpak</span>{/if}
						{#if p.installed}<span class="badge ok">Installed</span>{/if}
						<Icon name="chevron" size={16} />
					</button>
				{/each}
				{#if results.length > 100}<div class="muted small pad">Showing 100 of {results.length}. Search for something more specific.</div>{/if}
			</div>
		{/if}
	{/if}
</div>

<Modal title={detail?.name ?? 'Package'} bind:open={detailOpen}>
	{#if detail}
		{#if detail.summary}<p><strong>{detail.summary}</strong></p>{/if}
		{#if detail.description}<p class="desc">{detail.description}</p>{/if}
		<dl>
			<dt>Version</dt>
			<dd>{detail.version}{detail.installed_version && detail.installed_version !== detail.version ? ` (installed: ${detail.installed_version})` : ''}</dd>
			<dt>From</dt>
			<dd>{detail.backend === 'flatpak' ? 'Flatpak' : 'System packages'}{detail.repo ? ` · ${detail.repo}` : ''}</dd>
			{#if detail.size_bytes}<dt>Size</dt><dd>{bytes(detail.size_bytes)}</dd>{/if}
			{#if detail.license}<dt>Licence</dt><dd>{detail.license}</dd>{/if}
			{#if detail.url}<dt>Website</dt><dd><a href={detail.url} target="_blank" rel="noopener noreferrer">{detail.url}</a></dd>{/if}
		</dl>
		{#if detailError}<div class="notice bad">{detailError}</div>{/if}
	{/if}
	{#snippet actions()}
		<button onclick={() => (detailOpen = false)}>Close</button>
		{#if detail && session.isAdmin}
			{#if detail.installed}
				<button class="danger" onclick={() => detail && review('remove', detail)} disabled={job.active}>Remove…</button>
			{:else}
				<button class="primary" onclick={() => detail && review('install', detail)} disabled={job.active}>Install…</button>
			{/if}
		{/if}
	{/snippet}
</Modal>

<Modal title={plan?.action === 'remove' ? 'Remove these packages?' : 'Install these packages?'} bind:open={planOpen} wide>
	{#if plan}
		{#if plan.errors.length}
			<div class="notice bad">{#each plan.errors as e (e)}<div>{e}</div>{/each}</div>
		{/if}
		{#if plan.install.length}
			<div><strong>Installed ({plan.install.length})</strong>
				<ul>{#each plan.install as p (p.name + p.version)}<li>{p.name} <span class="muted small">{p.version}</span></li>{/each}</ul></div>
		{/if}
		{#if plan.update.length}
			<div><strong>Updated on the way ({plan.update.length})</strong>
				<ul>{#each plan.update as p (p.name + p.version)}<li>{p.name} <span class="muted small">{p.version}</span></li>{/each}</ul></div>
		{/if}
		{#if plan.remove.length}
			<div><strong class="bad">Removed ({plan.remove.length})</strong>
				<ul>{#each plan.remove as p (p.name + p.version)}<li>{p.name} <span class="muted small">{p.version}</span></li>{/each}</ul></div>
		{/if}
		{#if plan.warnings.length}
			<div class="notice warn">{#each plan.warnings as w (w)}<div>{w}</div>{/each}</div>
		{/if}
		{#if critical}
			<label class="check">
				<input type="checkbox" bind:checked={allowCritical} />
				I understand this can make the server unreachable or stop it from starting.
			</label>
		{/if}
	{/if}
	{#snippet actions()}
		<button onclick={() => (planOpen = false)}>Cancel</button>
		<button class={plan?.action === 'remove' ? 'danger solid' : 'primary'} onclick={apply} disabled={!plan || blocking}>
			{plan?.action === 'remove' ? 'Remove' : 'Install'}
		</button>
	{/snippet}
</Modal>

<style>
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
	form select {
		width: auto;
	}
	.list {
		padding: 0.3rem;
	}
	.item {
		display: flex;
		align-items: center;
		gap: 0.7rem;
		width: 100%;
		text-align: left;
		border: none;
		background: none;
		font-weight: 400;
		padding: 0.6rem 0.65rem;
		min-height: 0;
	}
	.item:hover {
		background: var(--bg-sunken);
	}
	.grow {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.grow .small {
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.name {
		font-weight: 600;
	}
	.ver {
		white-space: nowrap;
	}
	.pad {
		padding: 0.6rem;
	}
	.desc {
		white-space: pre-line;
		max-height: 30vh;
		overflow: auto;
	}
	dl {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.35rem 1rem;
		margin: 0;
		font-size: 0.9rem;
	}
	dt {
		color: var(--text-muted);
	}
	dd {
		margin: 0;
		word-break: break-word;
	}
	ul {
		margin: 0.3rem 0 0;
		padding-left: 1.2rem;
		max-height: 25vh;
		overflow: auto;
	}
	.bad {
		color: var(--bad);
	}
	.check {
		flex-direction: row;
		align-items: center;
		gap: 0.5rem;
		font-weight: 500;
	}
	.check input {
		width: auto;
		min-height: 0;
	}
	@media (max-width: 560px) {
		.ver {
			display: none;
		}
	}
</style>
