<script lang="ts">
	import { onMount } from 'svelte';
	import { get, post, errorText } from '#lib/api.ts';
	import type { AppDetail, ApplyPlan } from '#lib/types.ts';
	import type { JobRun } from '#lib/components/jobrun.svelte.ts';
	import Modal from '#lib/components/Modal.svelte';
	import Diff from '#lib/components/Diff.svelte';

	let {
		app,
		canChange,
		managed,
		job,
		onchange
	}: { app: AppDetail; canChange: boolean; managed: boolean; job: JobRun; onchange: () => void } = $props();

	let original = $state('');
	let text = $state('');
	let error = $state('');
	let loading = $state(true);
	let plan = $state<ApplyPlan | null>(null);
	let reviewing = $state(false);
	let message = $state('');
	let busy = $state(false);

	async function load() {
		loading = true;
		try {
			original = await get<string>(`/v1/apps/${encodeURIComponent(app.id)}/compose`);
			text = original;
			error = '';
		} catch (e) {
			error = errorText(e);
		} finally {
			loading = false;
		}
	}
	onMount(load);

	const editable = $derived(managed && canChange);
	const dirty = $derived(text !== original);
	const hasHidden = $derived(text.includes('***'));

	async function review() {
		busy = true;
		try {
			plan = await post<ApplyPlan>(`/v1/apps/${encodeURIComponent(app.id)}/apply`, { compose: text, dry_run: true });
			reviewing = true;
		} catch (e) {
			error = errorText(e);
		} finally {
			busy = false;
		}
	}

	async function apply() {
		reviewing = false;
		const done = await job.run(`Applying changes to ${app.title}`, () =>
			post(`/v1/apps/${encodeURIComponent(app.id)}/apply`, {
				compose: text,
				message: message.trim() || undefined
			})
		);
		if (done?.status === 'succeeded') {
			message = '';
			await load();
		}
		onchange();
	}
</script>

<div class="stack">
	{#if !managed}
		<div class="notice">
			{app.managed_by === 'casaos'
				? 'This app is still run by CasaOS. Adopt it into Hearth (hearth migrate casaos) to change it here.'
				: "Hearth doesn't manage this app, so its compose file is shown read-only."}
		</div>
	{:else if !canChange}
		<div class="notice">You can look at the setup, but only admins can change it.</div>
	{:else}
		<p class="muted small">
			The app's Docker Compose file. Passwords and other secrets live separately and are never shown here. Saving
			checks the file, shows you what changes, then restarts what's needed and rolls back if the app doesn't come
			up healthy.
		</p>
	{/if}

	{#if error}<div class="notice bad">{error}</div>{/if}
	{#if loading}
		<div class="card muted">Loading…</div>
	{:else}
		<textarea
			bind:value={text}
			readonly={!editable}
			spellcheck="false"
			autocapitalize="off"
			rows={Math.min(40, Math.max(12, text.split('\n').length + 1))}
			aria-label="Compose file"
		></textarea>
		{#if editable}
			{#if hasHidden}
				<div class="notice warn">
					The file contains <span class="mono">***</span> (a hidden value). Put the real value back or remove
					that line before saving.
				</div>
			{/if}
			<div class="row">
				<input placeholder="What did you change? (optional, kept in History)" bind:value={message} />
			</div>
			<div class="row">
				<button onclick={() => (text = original)} disabled={!dirty || busy}>Undo edits</button>
				<div class="spacer"></div>
				<button class="primary" onclick={review} disabled={!dirty || busy || job.active}>
					{busy ? 'Checking…' : 'Review changes'}
				</button>
			</div>
		{/if}
	{/if}
</div>

<Modal title="Review changes" bind:open={reviewing} wide>
	{#if plan}
		{#if plan.errors.length}
			<div class="notice bad">
				<strong>These need fixing first:</strong>
				<ul>{#each plan.errors as e (e)}<li>{e}</li>{/each}</ul>
			</div>
		{/if}
		{#if plan.unchanged}
			<div class="notice">Nothing would change.</div>
		{:else if plan.compose_diff}
			<Diff text={plan.compose_diff} />
		{/if}
		{#if plan.env_changes.length}
			<div class="small">Secret values changed: <span class="mono">{plan.env_changes.join(', ')}</span></div>
		{/if}
		{#if plan.decisions.length}
			<div class="notice">
				<strong>Hearth adjusted the file:</strong>
				<ul>{#each plan.decisions as d (d)}<li>{d}</li>{/each}</ul>
			</div>
		{/if}
		{#if plan.warnings.length}
			<div class="notice warn">
				<ul>{#each plan.warnings as w (w)}<li>{w}</li>{/each}</ul>
			</div>
		{/if}
	{/if}
	{#snippet actions()}
		<button onclick={() => (reviewing = false)}>Cancel</button>
		<button class="primary" onclick={apply} disabled={!plan || plan.errors.length > 0 || plan.unchanged}>Apply</button>
	{/snippet}
</Modal>

<style>
	textarea[readonly] {
		background: var(--bg-sunken);
	}
	ul {
		margin: 0.3rem 0 0;
		padding-left: 1.2rem;
	}
</style>
