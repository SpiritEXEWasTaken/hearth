<script lang="ts">
	import type { Snippet } from 'svelte';
	import Icon from './Icon.svelte';
	let {
		title,
		open = $bindable(false),
		children,
		actions,
		wide = false
	}: { title: string; open: boolean; children: Snippet; actions?: Snippet; wide?: boolean } = $props();
	let dialog: HTMLDialogElement | undefined = $state();
	const titleId = `dlg-${Math.random().toString(36).slice(2, 9)}`;
	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) dialog.showModal();
		if (!open && dialog.open) dialog.close();
	});
</script>

<dialog bind:this={dialog} class:wide aria-labelledby={titleId} onclose={() => (open = false)}>
	<header>
		<h2 id={titleId}>{title}</h2>
		<button class="ghost small" aria-label="Close" onclick={() => (open = false)}><Icon name="close" size={18} /></button>
	</header>
	<div class="body">{@render children()}</div>
	{#if actions}<footer>{@render actions()}</footer>{/if}
</dialog>

<style>
	dialog {
		border: 1px solid var(--border);
		border-radius: var(--radius);
		background: var(--bg-raised);
		color: var(--text);
		padding: 0;
		width: min(480px, calc(100vw - 1.5rem));
		max-height: calc(100dvh - 2rem);
		box-shadow: 0 20px 60px rgb(0 0 0 / 35%);
	}
	dialog.wide {
		width: min(860px, calc(100vw - 1.5rem));
	}
	dialog::backdrop {
		background: rgb(12 10 9 / 55%);
		backdrop-filter: blur(2px);
	}
	header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 0.9rem 1rem 0.4rem 1.1rem;
	}
	.body {
		padding: 0.4rem 1.1rem 1rem;
		overflow: auto;
		max-height: calc(100dvh - 10rem);
		display: flex;
		flex-direction: column;
		gap: 0.8rem;
	}
	footer {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		padding: 0.75rem 1.1rem 1rem;
		border-top: 1px solid var(--border);
		flex-wrap: wrap;
	}
</style>
