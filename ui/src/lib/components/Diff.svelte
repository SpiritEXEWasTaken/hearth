<script lang="ts">
	// A unified diff with added/removed lines coloured.
	let { text }: { text: string } = $props();
	const lines = $derived(text.split('\n'));
	function cls(l: string) {
		if (l.startsWith('+++') || l.startsWith('---')) return 'file';
		if (l.startsWith('@@')) return 'hunk';
		if (l.startsWith('+')) return 'add';
		if (l.startsWith('-')) return 'del';
		return '';
	}
</script>

<pre class="diff">{#each lines as l, i (i)}<span class={cls(l)}>{l}
</span>{/each}</pre>

<style>
	.diff {
		max-height: 50vh;
		white-space: pre;
	}
	.add {
		background: var(--ok-soft);
		color: var(--ok);
		display: block;
	}
	.del {
		background: var(--bad-soft);
		color: var(--bad);
		display: block;
	}
	.hunk {
		color: var(--info);
		display: block;
	}
	.file {
		color: var(--text-muted);
		display: block;
	}
</style>
