<script lang="ts">
	import { onMount } from 'svelte';
	import { get, post, errorText } from '#lib/api.ts';
	import type { AppDetail, BackupInfo } from '#lib/types.ts';
	import type { JobRun } from '#lib/components/jobrun.svelte.ts';
	import { bytes, dateTime } from '#lib/format.ts';
	import Modal from '#lib/components/Modal.svelte';
	import Icon from '#lib/components/Icon.svelte';

	let { app, canChange, job }: { app: AppDetail; canChange: boolean; job: JobRun } = $props();
	let list = $state<BackupInfo[] | null>(null);
	let error = $state('');
	let stop = $state(true);
	let restoring = $state<BackupInfo | null>(null);
	let confirmOpen = $state(false);

	async function load() {
		try {
			list = (await get<BackupInfo[]>(`/v1/apps/${encodeURIComponent(app.id)}/backups`)).sort(
				(a, b) => b.created_at - a.created_at
			);
		} catch (e) {
			error = errorText(e);
		}
	}
	onMount(load);

	async function backup() {
		await job.run(`Backing up ${app.title}`, () => post(`/v1/apps/${encodeURIComponent(app.id)}/backup`, { stop }));
		await load();
	}

	function askRestore(b: BackupInfo) {
		restoring = b;
		confirmOpen = true;
	}

	async function restore() {
		const b = restoring;
		confirmOpen = false;
		if (!b) return;
		await job.run(`Restoring ${app.title}`, () =>
			post(`/v1/apps/${encodeURIComponent(app.id)}/restore`, { file: b.file })
		);
		await load();
	}
</script>

<div class="stack">
	{#if canChange}
		<div class="card row">
			<div class="grow">
				<h2>Back up now</h2>
				<div class="muted small">Saves the app's setup and data. Hearth keeps the newest few backups per app.</div>
			</div>
			<label class="inline"><input type="checkbox" bind:checked={stop} /> Stop the app while copying (safer for databases)</label>
			<button class="primary" onclick={backup} disabled={job.active}><Icon name="backup" size={18} /> Back up</button>
		</div>
	{/if}
	{#if error}<div class="notice bad">{error}</div>{/if}
	<div class="card">
		{#if list === null}
			<span class="muted">Loading…</span>
		{:else if list.length === 0}
			<span class="muted">No backups yet.</span>
		{:else}
			<div class="table-wrap">
				<table>
					<thead><tr><th>Made</th><th>Size</th><th></th></tr></thead>
					<tbody>
						{#each list as b (b.file)}
							<tr>
								<td>{dateTime(b.created_at)}<div class="muted small mono">{b.file}</div></td>
								<td>{bytes(b.size_bytes)}</td>
								<td class="right">
									{#if canChange}<button class="small" onclick={() => askRestore(b)} disabled={job.active}>Restore</button>{/if}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</div>
</div>

<Modal title="Restore this backup?" bind:open={confirmOpen}>
	{#if restoring}
		<p>
			{app.title} goes back to how it was on <strong>{dateTime(restoring.created_at)}</strong>. Its current data is moved
			aside, not deleted.
		</p>
	{/if}
	{#snippet actions()}
		<button onclick={() => (confirmOpen = false)}>Cancel</button>
		<button class="primary" onclick={restore}>Restore</button>
	{/snippet}
</Modal>

<style>
	.grow {
		flex: 1;
		min-width: 200px;
	}
	.inline {
		flex-direction: row;
		align-items: center;
		gap: 0.4rem;
		font-weight: 500;
	}
	.inline input {
		width: auto;
		min-height: 0;
	}
	.right {
		text-align: right;
	}
</style>
