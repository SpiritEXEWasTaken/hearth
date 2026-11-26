<script lang="ts">
	import { post, errorText } from '#lib/api.ts';
	import type { AppDetail, UpdatePlan } from '#lib/types.ts';
	import type { JobRun } from '#lib/components/jobrun.svelte.ts';
	import { ago } from '#lib/format.ts';
	import Modal from '#lib/components/Modal.svelte';
	import Icon from '#lib/components/Icon.svelte';

	let {
		app,
		canChange,
		job,
		onchange
	}: { app: AppDetail; canChange: boolean; job: JobRun; onchange: () => void } = $props();

	let checking = $state(false);
	let error = $state('');
	let plan = $state<UpdatePlan | null>(null);
	let confirming = $state(false);

	const available = $derived(app.images.filter((i) => i.latest?.update_available));
	const majors = $derived(app.images.filter((i) => i.latest?.newer_major));
	const lastCheck = $derived(Math.max(0, ...app.images.map((i) => i.latest?.checked_at ?? 0)));

	async function check() {
		checking = true;
		error = '';
		try {
			await post(`/v1/versions/check?app=${encodeURIComponent(app.id)}`, {});
			onchange();
		} catch (e) {
			error = errorText(e);
		} finally {
			checking = false;
		}
	}

	async function review() {
		error = '';
		try {
			plan = await post<UpdatePlan>(`/v1/apps/${encodeURIComponent(app.id)}/update`, { dry_run: true });
			confirming = true;
		} catch (e) {
			error = errorText(e);
		}
	}

	async function update() {
		confirming = false;
		await job.run(`Updating ${app.title}`, () => post(`/v1/apps/${encodeURIComponent(app.id)}/update`, {}));
		onchange();
	}
</script>

<div class="stack">
	<div class="card stack">
		<div class="row">
			<div>
				<h2>{available.length ? 'An update is ready' : 'Up to date'}</h2>
				<div class="muted small">Last checked {ago(lastCheck || null)}</div>
			</div>
			<div class="spacer"></div>
			<button onclick={check} disabled={checking}><Icon name="restart" size={16} /> {checking ? 'Checking…' : 'Check now'}</button>
			{#if available.length && canChange}
				<button class="primary" onclick={review} disabled={job.active}><Icon name="update" size={18} /> Update</button>
			{/if}
		</div>
		{#if error}<div class="notice bad">{error}</div>{/if}
		{#if available.length}
			<ul class="plain">
				{#each available as i (i.service)}
					<li>
						<strong>{i.service}</strong>: {i.running_version ?? 'current build'} → {i.latest?.version ?? 'newer build of the same version'}
					</li>
				{/each}
			</ul>
			<p class="muted small">
				Updating keeps the same version line (the tag in the compose file). Hearth keeps the old image, checks the
				app comes up healthy, and switches back automatically if it doesn't.
			</p>
		{/if}
	</div>

	{#if majors.length}
		<div class="notice warn">
			<strong>New major versions exist:</strong>
			{#each majors as i (i.service)}
				<div>{i.service}: {i.latest?.newer_major}</div>
			{/each}
			<div class="small">
				These often need migration steps, so Hearth never switches to them by itself. Read the app's release notes
				first; then change the tag in Settings.
			</div>
		</div>
	{/if}
</div>

<Modal title="Update {app.title}?" bind:open={confirming}>
	{#if plan}
		<ul class="plain">
			{#each plan.services as s (s.service)}
				<li><strong>{s.service}</strong>: {s.running_version ?? 'current'} → {s.new_version ?? 'newer build'}</li>
			{/each}
		</ul>
		{#if plan.backup_first}
			<div class="notice">A database changes, so Hearth makes a backup first.</div>
		{/if}
		<p class="muted small">The app restarts during the update.</p>
	{/if}
	{#snippet actions()}
		<button onclick={() => (confirming = false)}>Cancel</button>
		<button class="primary" onclick={update}>Update now</button>
	{/snippet}
</Modal>

<style>
	.plain {
		margin: 0;
		padding-left: 1.1rem;
	}
</style>
