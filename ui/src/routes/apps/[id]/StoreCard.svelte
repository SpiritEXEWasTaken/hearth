<script lang="ts">
	// Store details for an installed app: notes from its store entry, sign-in details Hearth
	// generated, or a suggestion to link it to a matching store entry.
	import { onMount } from 'svelte';
	import { get, post, errorText } from '#lib/api.ts';
	import type { AppDetail, AppStoreInfo } from '#lib/types.ts';
	import { session } from '#lib/session.svelte.ts';
	let { app, justInstalled = false }: { app: AppDetail; justInstalled?: boolean } = $props();

	let info = $state<AppStoreInfo | null>(null);
	let creds = $state<{ label: string; value: string }[] | null>(null);
	let error = $state('');
	let busy = $state(false);

	async function load() {
		try {
			info = await get<AppStoreInfo>(`/v1/apps/${encodeURIComponent(app.id)}/store`);
		} catch {
			info = null;
		}
	}
	onMount(load);

	async function showCreds() {
		try {
			creds = await get(`/v1/apps/${encodeURIComponent(app.id)}/credentials`);
		} catch (e) {
			error = errorText(e);
		}
	}

	async function link(entry: string | null) {
		busy = true;
		try {
			await post(`/v1/apps/${encodeURIComponent(app.id)}/store`, { entry });
			await load();
		} catch (e) {
			error = errorText(e);
		} finally {
			busy = false;
		}
	}
</script>

{#if info && (info.entry || (info.suggestion && session.isAdmin))}
	<div class="card stack" data-testid="store-card">
		<h2>From the store</h2>
		{#if error}<div class="notice bad">{error}</div>{/if}
		{#if info.entry}
			<p class="small">
				<a href={`/store/${encodeURIComponent(info.entry)}`}>{info.name ?? info.entry}</a>
				{info.how === 'linked' ? '(linked by you)' : ''}
			</p>
			{#if justInstalled}<div class="notice ok">Installed. Here's what to do next.</div>{/if}
			{#if info.notice}<div class="notice warn small">{info.notice}</div>{/if}
			{#if info.after_install.length}
				<ul class="small">
					{#each info.after_install as line, i (i)}<li>{line}</li>{/each}
				</ul>
			{/if}
			{#if info.credentials.length && session.isAdmin}
				{#if creds}
					<dl class="creds">
						{#each creds as c (c.label)}
							<dt>{c.label}</dt>
							<dd class="mono">{c.value}</dd>
						{/each}
					</dl>
					<button class="small" onclick={() => (creds = null)}>Hide</button>
				{:else}
					<button class="small" onclick={showCreds}>Show sign-in details</button>
				{/if}
			{/if}
			{#if info.how === 'linked' && session.isAdmin}
				<button class="ghost small" onclick={() => link(null)} disabled={busy}>Unlink</button>
			{/if}
		{:else if info.suggestion}
			<p class="small">
				This looks like <a href={`/store/${encodeURIComponent(info.suggestion.id)}`}>{info.suggestion.name}</a> from
				the store. Linking shows its notes and tips here. Nothing in the app changes.
			</p>
			<button class="small" onclick={() => link(info!.suggestion!.id)} disabled={busy}>Link it</button>
		{/if}
	</div>
{/if}

<style>
	ul {
		margin: 0;
		padding-left: 1.1rem;
	}
	li + li {
		margin-top: 0.3rem;
	}
	.creds {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.3rem 0.8rem;
		margin: 0;
		overflow-wrap: anywhere;
	}
	.creds dt {
		color: var(--text-muted);
	}
	.creds dd {
		margin: 0;
	}
	p {
		margin: 0;
	}
</style>
