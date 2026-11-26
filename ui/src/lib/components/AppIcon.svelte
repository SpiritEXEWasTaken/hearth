<script lang="ts">
	import type { App } from '#lib/types.ts';
	let { app, size = 56 }: { app: App; size?: number } = $props();
	let failed = $state(false);
	const hue = $derived([...app.id].reduce((h, c) => (h * 31 + c.charCodeAt(0)) % 360, 7));
</script>

{#if app.icon && !failed}
	<img
		src={`/v1/apps/${encodeURIComponent(app.id)}/icon`}
		alt=""
		width={size}
		height={size}
		style:width="{size}px"
		style:height="{size}px"
		loading="lazy"
		onerror={() => (failed = true)}
	/>
{:else}
	<span
		class="letter"
		style:width="{size}px"
		style:height="{size}px"
		style:font-size="{size * 0.45}px"
		style:background="hsl({hue} 55% 45%)">{app.title.slice(0, 1).toUpperCase()}</span
	>
{/if}

<style>
	img {
		object-fit: contain;
		border-radius: 22%;
		display: block;
	}
	.letter {
		display: grid;
		place-items: center;
		border-radius: 22%;
		color: #fff;
		font-weight: 700;
	}
</style>
