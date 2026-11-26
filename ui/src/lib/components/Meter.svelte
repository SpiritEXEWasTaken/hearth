<script lang="ts">
	// A widget with a ring gauge: label, big value, detail line.
	let {
		label,
		value,
		detail = '',
		percent,
		icon = ''
	}: { label: string; value: string; detail?: string; percent: number; icon?: string } = $props();
	import Icon from './Icon.svelte';
	const r = 22;
	const c = 2 * Math.PI * r;
	const tone = $derived(percent >= 90 ? 'var(--bad)' : percent >= 75 ? 'var(--warn)' : 'var(--accent)');
</script>

<div class="meter card">
	<svg class="ring" viewBox="0 0 56 56" width="56" height="56" aria-hidden="true">
		<circle cx="28" cy="28" {r} fill="none" stroke="var(--bg-sunken)" stroke-width="6" />
		<circle
			cx="28"
			cy="28"
			{r}
			fill="none"
			stroke={tone}
			stroke-width="6"
			stroke-linecap="round"
			stroke-dasharray={c}
			stroke-dashoffset={c * (1 - Math.min(100, Math.max(0, percent)) / 100)}
			transform="rotate(-90 28 28)"
		/>
	</svg>
	<div class="text">
		<div class="label">
			{#if icon}<Icon name={icon} size={14} />{/if}
			{label}
		</div>
		<div class="value">{value}</div>
		{#if detail}<div class="detail">{detail}</div>{/if}
	</div>
</div>

<style>
	.meter {
		display: flex;
		align-items: center;
		gap: 0.85rem;
		padding: 0.85rem 1rem;
		min-width: 0;
	}
	circle {
		transition: stroke-dashoffset 0.4s ease;
	}
	.text {
		min-width: 0;
	}
	.label {
		display: flex;
		align-items: center;
		gap: 0.3rem;
		color: var(--text-muted);
		font-size: 0.78rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}
	.value {
		font-size: 1.35rem;
		font-weight: 700;
		font-variant-numeric: tabular-nums;
	}
	.detail {
		color: var(--text-muted);
		font-size: 0.8rem;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
</style>
