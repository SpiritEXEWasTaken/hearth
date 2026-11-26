<script lang="ts">
	// Shows a running change (start, update, apply…) with its live log and outcome.
	import type { Job } from '#lib/api.ts';
	let { title, lines, job, error }: { title: string; lines: string[]; job: Job | null; error: string } =
		$props();
	let box: HTMLPreElement | undefined = $state();
	$effect(() => {
		void lines.length;
		if (box) box.scrollTop = box.scrollHeight;
	});
	const outcome = $derived.by(() => {
		if (error) return { tone: 'bad', text: error };
		if (!job) return { tone: 'info', text: 'Working…' };
		switch (job.status) {
			case 'running':
				return { tone: 'info', text: 'Working…' };
			case 'succeeded':
				return { tone: 'ok', text: 'Done.' };
			case 'rolled_back':
				return {
					tone: 'warn',
					text: `It didn't work, so Hearth put the app back the way it was. ${job.error ?? ''}`
				};
			case 'rollback_failed':
				return {
					tone: 'bad',
					text: `It failed, and putting it back failed too. The app needs attention. ${job.error ?? ''}`
				};
			default:
				return { tone: 'bad', text: job.error ?? 'Failed.' };
		}
	});
</script>

<div class="stack">
	<div class="row"><strong>{title}</strong></div>
	<div class="notice {outcome.tone}" role="status">{outcome.text}</div>
	{#if lines.length}
		<pre bind:this={box} class="log">{lines.join('\n')}</pre>
	{/if}
</div>

<style>
	.log {
		max-height: 260px;
	}
</style>
