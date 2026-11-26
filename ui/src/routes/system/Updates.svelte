<script lang="ts">
	import { onMount } from 'svelte';
	import { get, post, put, errorText } from '#lib/api.ts';
	import { session } from '#lib/session.svelte.ts';
	import type { SystemState, SystemUpdate, UpgradePlan, Setting } from '#lib/types.ts';
	import type { JobRun } from '#lib/components/jobrun.svelte.ts';
	import { ago } from '#lib/format.ts';
	import Icon from '#lib/components/Icon.svelte';
	import Modal from '#lib/components/Modal.svelte';

	let { job }: { job: JobRun } = $props();
	let sys = $state<SystemState | null>(null);
	let error = $state('');
	let selected = $state<string[]>([]);
	let plan = $state<UpgradePlan | null>(null);
	let planOpen = $state(false);
	let schedule = $state('off');
	let scheduleTime = $state('04:00');
	let scheduleMsg = $state('');
	let showAll = $state(false);

	async function load() {
		try {
			sys = await get<SystemState>('/v1/system');
			error = '';
			const keep = new Set(sys.updates.updates.map((u) => u.id));
			selected = selected.filter((id) => keep.has(id));
		} catch (e) {
			error = errorText(e);
		}
	}

	async function loadSchedule() {
		try {
			const s = await get<Record<string, Setting>>('/v1/settings');
			const v = s.security_updates?.value ?? 'off';
			schedule = v === 'off' ? 'off' : 'daily';
			if (v !== 'off') scheduleTime = v;
		} catch {
			/* read-only users see the default */
		}
	}

	onMount(() => {
		load();
		loadSchedule();
	});

	async function check() {
		await job.run('Checking for system updates', () => post('/v1/system/check', {}));
		await load();
	}

	async function review(body: { ids?: string[]; all?: boolean; security_only?: boolean }) {
		error = '';
		try {
			plan = await post<UpgradePlan>('/v1/system/upgrade', { ...body, dry_run: true });
			planBody = body;
			planOpen = true;
		} catch (e) {
			error = errorText(e);
		}
	}
	let planBody: { ids?: string[]; all?: boolean; security_only?: boolean } = {};

	async function install() {
		planOpen = false;
		await job.run('Installing system updates', () => post('/v1/system/upgrade', planBody));
		selected = [];
		await load();
	}

	async function saveSchedule() {
		scheduleMsg = '';
		try {
			await put('/v1/settings', { key: 'security_updates', value: schedule === 'off' ? 'off' : scheduleTime });
			scheduleMsg = schedule === 'off' ? 'Saved: updates are only listed.' : `Saved: security updates install every day at ${scheduleTime}.`;
		} catch (e) {
			scheduleMsg = errorText(e);
		}
	}

	const updates = $derived(
		[...(sys?.updates.updates ?? [])].sort(
			(a, b) => Number(b.security) - Number(a.security) || a.name.localeCompare(b.name)
		)
	);
	const security = $derived(updates.filter((u) => u.security));
	const shown = $derived(showAll || updates.length <= 40 ? updates : updates.slice(0, 40));
	const reboot = $derived(sys?.info.reboot ?? sys?.updates.reboot);
	const backendNames: Record<string, string> = { packagekit: 'System packages', pacman: 'pacman', flatpak: 'Flatpak' };

	function toggleAll(on: boolean) {
		selected = on ? updates.map((u) => u.id) : [];
	}
	function sevTone(u: SystemUpdate) {
		if (u.security) return 'bad';
		if (u.severity === 'critical' || u.severity === 'important') return 'warn';
		return '';
	}
	function sevText(u: SystemUpdate) {
		if (u.security) return 'Security';
		if (u.severity === 'new') return 'New package';
		if (['unknown', 'normal', 'enhancement', 'low'].includes(u.severity)) return '';
		return u.severity.charAt(0).toUpperCase() + u.severity.slice(1);
	}
</script>

<div class="stack">
	{#if error}<div class="notice bad">{error}</div>{/if}

	{#if reboot?.needed}
		<div class="notice warn">
			<strong>A restart is needed</strong> to finish installing updates.
			{#each reboot.reasons as r (r)}<div class="small">{r}</div>{/each}
			<div class="small">Hearth never restarts the server by itself. Restart it when it suits you.</div>
		</div>
	{/if}

	{#if !sys}
		<div class="card muted">Loading…</div>
	{:else}
		<div class="card stack">
			<div class="row">
				<div class="grow">
					<h2>
						{#if updates.length === 0}Up to date{:else}{updates.length} update{updates.length === 1 ? '' : 's'} available{/if}
					</h2>
					<div class="muted small">
						{sys.info.distro_name} · kernel {sys.info.kernel} · checked {ago(sys.updates.checked_at || null)}
					</div>
				</div>
				<button onclick={check} disabled={job.active}><Icon name="restart" size={16} /> Check now</button>
			</div>
			{#if sys.updates.errors.length}
				<div class="notice warn">{#each sys.updates.errors as e (e)}<div>{e}</div>{/each}</div>
			{/if}
			{#if updates.length && session.isAdmin}
				<div class="row">
					{#if security.length}
						<button class="primary" onclick={() => review({ security_only: true })} disabled={job.active}>
							<Icon name="shield" size={16} /> Install {security.length} security update{security.length === 1 ? '' : 's'}
						</button>
					{/if}
					<button onclick={() => review({ all: true })} disabled={job.active} class:primary={!security.length}>Install all</button>
					{#if selected.length}
						<button onclick={() => review({ ids: selected })} disabled={job.active}>Install {selected.length} selected</button>
					{/if}
				</div>
			{/if}
		</div>

		{#if updates.length}
			<div class="card list">
				{#if session.isAdmin}
					<label class="selall">
						<input type="checkbox" checked={selected.length === updates.length} onchange={(e) => toggleAll((e.currentTarget as HTMLInputElement).checked)} />
						Select all
					</label>
				{/if}
				{#each shown as u (u.id)}
					<label class="upd">
						{#if session.isAdmin}<input type="checkbox" value={u.id} bind:group={selected} />{/if}
						<span class="grow">
							<span class="name">{u.name}</span>
							<span class="muted small">
								{u.current_version ? `${u.current_version} → ` : ''}{u.new_version}
								{#if u.repo}· {u.repo}{/if}
								{#if u.backend !== 'packagekit'}· {backendNames[u.backend] ?? u.backend}{/if}
							</span>
							{#if u.summary}<span class="muted small summary">{u.summary}</span>{/if}
						</span>
						{#if sevText(u)}<span class="badge {sevTone(u)}">{sevText(u)}</span>{/if}
					</label>
				{/each}
				{#if shown.length < updates.length}
					<button class="ghost more" onclick={() => (showAll = true)}>Show all {updates.length}</button>
				{/if}
			</div>
		{/if}

		<div class="grid">
			<div class="card stack">
				<h2>Automatic security updates</h2>
				<p class="muted small">
					Installs security updates once a day. Other updates are only listed. Hearth never restarts the server by
					itself, even when an update needs it.
				</p>
				{#if session.isAdmin}
					<div class="row">
						<select bind:value={schedule} aria-label="Automatic security updates">
							<option value="off">Off (only list them)</option>
							<option value="daily">Every day at</option>
						</select>
						{#if schedule === 'daily'}<input type="time" bind:value={scheduleTime} aria-label="Time" />{/if}
						<button onclick={saveSchedule}>Save</button>
					</div>
					{#if scheduleMsg}<div class="small muted" role="status">{scheduleMsg}</div>{/if}
				{:else}
					<div>{schedule === 'off' ? 'Off' : `Every day at ${scheduleTime}`}</div>
				{/if}
			</div>
			<div class="card stack">
				<h2>Where updates come from</h2>
				{#each sys.info.backends as b (b.kind)}
					<div class="row small">
						<span class="badge {b.available ? 'ok' : ''}">{b.available ? 'On' : 'Off'}</span>
						<strong>{backendNames[b.kind] ?? b.kind}</strong>
						<span class="muted">{b.detail}{b.note ? ` · ${b.note}` : ''}</span>
					</div>
				{/each}
			</div>
		</div>
	{/if}
</div>

<Modal title="Install these updates?" bind:open={planOpen} wide>
	{#if plan}
		<p>{plan.updates.length} update{plan.updates.length === 1 ? '' : 's'} will be installed:</p>
		<ul class="plan">
			{#each plan.updates as u (u.id)}
				<li>{u.name} <span class="muted small">{u.new_version}</span>{#if u.security} <span class="badge bad">Security</span>{/if}</li>
			{/each}
		</ul>
		{#if plan.notes.length}
			<div class="notice">{#each plan.notes as n (n)}<div>{n}</div>{/each}</div>
		{/if}
	{/if}
	{#snippet actions()}
		<button onclick={() => (planOpen = false)}>Cancel</button>
		<button class="primary" onclick={install} disabled={!plan?.updates.length}>Install</button>
	{/snippet}
</Modal>

<style>
	.grow {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.list {
		padding: 0.3rem;
	}
	.selall {
		flex-direction: row;
		align-items: center;
		gap: 0.6rem;
		padding: 0.5rem 0.65rem;
		border-bottom: 1px solid var(--border);
		font-weight: 500;
	}
	.upd {
		flex-direction: row;
		align-items: center;
		gap: 0.7rem;
		padding: 0.55rem 0.65rem;
		font-weight: 400;
		border-radius: var(--radius-sm);
		cursor: pointer;
	}
	.upd:hover {
		background: var(--bg-sunken);
	}
	input[type='checkbox'] {
		width: 18px;
		height: 18px;
		min-height: 0;
		flex: none;
	}
	.name {
		font-weight: 600;
	}
	.summary {
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.more {
		width: 100%;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
		gap: 1rem;
	}
	.row select,
	.row input[type='time'] {
		width: auto;
	}
	.plan {
		max-height: 40vh;
		overflow: auto;
		margin: 0;
		padding-left: 1.2rem;
	}
</style>
