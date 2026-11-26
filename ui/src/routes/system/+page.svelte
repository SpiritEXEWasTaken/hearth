<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import Icon from '#lib/components/Icon.svelte';
	import Modal from '#lib/components/Modal.svelte';
	import JobPanel from '#lib/components/JobPanel.svelte';
	import { JobRun } from '#lib/components/jobrun.svelte.ts';
	import Updates from './Updates.svelte';
	import Packages from './Packages.svelte';

	const job = new JobRun();
	const tab = $derived(page.url.searchParams.get('tab') === 'packages' ? 'packages' : 'updates');
	function setTab(t: string) {
		goto(t === 'updates' ? '/system' : `/system?tab=${t}`, { replace: true, reset: false });
	}
</script>

<svelte:head><title>System · Hearth</title></svelte:head>

<h1 class="title">System</h1>

<div class="tabs" role="tablist">
	<button role="tab" aria-selected={tab === 'updates'} class:on={tab === 'updates'} onclick={() => setTab('updates')}>
		<Icon name="update" size={16} /> Updates
	</button>
	<button role="tab" aria-selected={tab === 'packages'} class:on={tab === 'packages'} onclick={() => setTab('packages')}>
		<Icon name="box" size={16} /> Packages
	</button>
</div>

{#if tab === 'packages'}
	<Packages {job} />
{:else}
	<Updates {job} />
{/if}

<Modal title={job.title || 'Working'} bind:open={job.open} wide>
	<JobPanel title="" lines={job.lines} job={job.job} error={job.error} />
	{#snippet actions()}
		<button class="primary" onclick={() => (job.open = false)} disabled={job.active}>{job.active ? 'Working…' : 'Close'}</button>
	{/snippet}
</Modal>

<style>
	.title {
		margin-bottom: 0.8rem;
	}
	.tabs {
		display: flex;
		gap: 0.25rem;
		border-bottom: 1px solid var(--border);
		margin-bottom: 1rem;
	}
	.tabs button {
		border: none;
		background: none;
		border-radius: 0;
		border-bottom: 2px solid transparent;
		color: var(--text-muted);
		padding: 0.55rem 0.8rem;
	}
	.tabs button.on {
		color: var(--accent-strong);
		border-bottom-color: var(--accent);
	}
</style>
