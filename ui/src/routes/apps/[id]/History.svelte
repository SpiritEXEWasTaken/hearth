<script lang="ts">
	import { onMount } from 'svelte';
	import { get, post, errorText } from '#lib/api.ts';
	import type { AppDetail, ApplyPlan, HistoryEntry } from '#lib/types.ts';
	import type { JobRun } from '#lib/components/jobrun.svelte.ts';
	import { dateTime } from '#lib/format.ts';
	import Modal from '#lib/components/Modal.svelte';
	import Diff from '#lib/components/Diff.svelte';

	let {
		app,
		canChange,
		job,
		onchange
	}: { app: AppDetail; canChange: boolean; job: JobRun; onchange: () => void } = $props();
	let list = $state<HistoryEntry[] | null>(null);
	let error = $state('');
	let target = $state<HistoryEntry | null>(null);
	let plan = $state<ApplyPlan | null>(null);
	let open = $state(false);

	async function load() {
		try {
			list = (await get<HistoryEntry[]>(`/v1/apps/${encodeURIComponent(app.id)}/history`)).sort(
				(a, b) => b.revision - a.revision
			);
		} catch (e) {
			error = errorText(e);
		}
	}
	onMount(load);

	async function ask(h: HistoryEntry) {
		error = '';
		try {
			plan = await post<ApplyPlan>(`/v1/apps/${encodeURIComponent(app.id)}/revert`, {
				revision: h.revision,
				dry_run: true
			});
			target = h;
			open = true;
		} catch (e) {
			error = errorText(e);
		}
	}

	async function revert() {
		const h = target;
		open = false;
		if (!h) return;
		await job.run(`Going back to revision ${h.revision}`, () =>
			post(`/v1/apps/${encodeURIComponent(app.id)}/revert`, { revision: h.revision })
		);
		await load();
		onchange();
	}

	function who(actor: string) {
		return actor.replace(/^user:/, '').replace(/^token:/, 'token ').replace(/^uid:0$/, 'root');
	}
</script>

<div class="stack">
	<p class="muted small">Every change to this app's setup is saved here. You can go back to any of them.</p>
	{#if error}<div class="notice bad">{error}</div>{/if}
	<div class="card">
		{#if list === null}
			<span class="muted">Loading…</span>
		{:else if list.length === 0}
			<span class="muted">No history yet.</span>
		{:else}
			<ol class="timeline">
				{#each list as h, i (h.revision)}
					<li>
						<div class="dot" class:current={i === 0}></div>
						<div class="body">
							<div class="row">
								<strong>{h.message}</strong>
								{#if i === 0}<span class="badge ok">Current</span>{/if}
							</div>
							<div class="muted small">#{h.revision} · {dateTime(h.time)} · {who(h.actor)}</div>
						</div>
						{#if canChange && i > 0}
							<button class="small" onclick={() => ask(h)} disabled={job.active}>Go back to this</button>
						{/if}
					</li>
				{/each}
			</ol>
		{/if}
	</div>
</div>

<Modal title="Go back to revision {target?.revision}?" bind:open wide>
	{#if plan}
		{#if plan.unchanged}
			<div class="notice">This revision is the same as now.</div>
		{:else if plan.compose_diff}
			<Diff text={plan.compose_diff} />
		{/if}
		{#if plan.env_changes.length}<div class="small">Secret values differ too.</div>{/if}
	{/if}
	{#snippet actions()}
		<button onclick={() => (open = false)}>Cancel</button>
		<button class="primary" onclick={revert} disabled={!plan || plan.unchanged}>Go back</button>
	{/snippet}
</Modal>

<style>
	.timeline {
		list-style: none;
		margin: 0;
		padding: 0;
	}
	.timeline li {
		display: flex;
		align-items: center;
		gap: 0.8rem;
		padding: 0.6rem 0.2rem;
		position: relative;
	}
	.timeline li + li {
		border-top: 1px solid var(--border);
	}
	.dot {
		width: 11px;
		height: 11px;
		border-radius: 50%;
		background: var(--border);
		flex: none;
	}
	.dot.current {
		background: var(--ok);
	}
	.body {
		flex: 1;
		min-width: 0;
	}
</style>
