<script lang="ts">
	import { onMount } from 'svelte';
	import { get, errorText } from '#lib/api.ts';
	import { session } from '#lib/session.svelte.ts';
	import type { Status, App, SystemState } from '#lib/types.ts';
	import { bytes, percent, duration, openUrl, updateCount, stateLabel, kindLabel } from '#lib/format.ts';
	import Meter from '#lib/components/Meter.svelte';
	import AppIcon from '#lib/components/AppIcon.svelte';
	import Icon from '#lib/components/Icon.svelte';

	let status = $state<Status | null>(null);
	let error = $state('');

	async function load() {
		try {
			status = await get<Status>('/v1/status');
			error = '';
		} catch (e) {
			error = errorText(e);
		}
	}

	let sys = $state<SystemState | null>(null);

	onMount(() => {
		load();
		// Once per visit: the update list only changes when it's checked (twice a day or by hand).
		get<SystemState>('/v1/system')
			.then((v) => (sys = v))
			.catch(() => {});
		const t = setInterval(() => {
			if (document.visibilityState === 'visible') load();
		}, 5000);
		return () => clearInterval(t);
	});

	const tiles = $derived((status?.apps ?? []).filter((a) => a.dashboard).sort(byTitle));
	const others = $derived((status?.apps ?? []).filter((a) => !a.dashboard).sort(byTitle));
	const running = $derived((status?.apps ?? []).filter((a) => a.state === 'running').length);
	const updates = $derived((status?.apps ?? []).reduce((n, a) => n + (updateCount(a) > 0 ? 1 : 0), 0));
	const disk = $derived.by(() => {
		const d = status?.host.disks ?? [];
		if (!d.length) return null;
		return [...d].sort((a, b) => percent(b.used_bytes, b.total_bytes) - percent(a.used_bytes, a.total_bytes))[0];
	});

	function byTitle(a: App, b: App) {
		return a.title.localeCompare(b.title);
	}
</script>

<svelte:head><title>Home · Hearth</title></svelte:head>

{#if error && !status}
	<div class="notice bad">{error}</div>
{:else if !status}
	<div class="widgets">
		{#each [1, 2, 3, 4] as i (i)}<div class="card skeleton"></div>{/each}
	</div>
{:else}
	{@const h = status.host}
	<section class="widgets" aria-label="Server">
		<Meter
			icon="cpu"
			label="CPU"
			value="{h.cpu_percent.toFixed(0)}%"
			detail="{h.cpu_count} cores · load {h.load[0].toFixed(2)}"
			percent={h.cpu_percent}
		/>
		<Meter
			icon="memory"
			label="Memory"
			value={bytes(h.memory.used_bytes)}
			detail="of {bytes(h.memory.total_bytes)}{h.memory.swap_used_bytes > 0
				? ` · swap ${bytes(h.memory.swap_used_bytes)}`
				: ''}"
			percent={percent(h.memory.used_bytes, h.memory.total_bytes)}
		/>
		{#if disk}
			<Meter
				icon="disk"
				label="Storage"
				value="{percent(disk.used_bytes, disk.total_bytes).toFixed(0)}%"
				detail="{bytes(disk.available_bytes)} free on {disk.mount}"
				percent={percent(disk.used_bytes, disk.total_bytes)}
			/>
		{/if}
		<a class="card summary" href="/apps">
			<div class="label"><Icon name="apps" size={14} /> Apps</div>
			<div class="value">{running}<span class="muted">{' '}of {status.apps.length} running</span></div>
			<div class="detail">
				{#if updates > 0}
					<span class="badge accent"><Icon name="update" size={12} /> {updates} with updates</span>
				{:else}
					<span class="muted">Up {duration(h.uptime_secs)}</span>
				{/if}
			</div>
		</a>
	</section>

	{#if sys && (sys.updates.updates.length || sys.info.reboot.needed)}
		{@const n = sys.updates.updates.length}
		{@const sec = sys.updates.updates.filter((u) => u.security).length}
		<a class="sysline card" class:warn={sys.info.reboot.needed || sec > 0} href="/system">
			<Icon name="shield" size={20} />
			<span class="grow">
				{#if n}
					<strong>{n} system update{n === 1 ? '' : 's'}</strong>{#if sec}<span>, {sec} for security</span>{/if}
				{/if}
				{#if sys.info.reboot.needed}
					<span class="badge warn">Restart needed</span>
				{/if}
			</span>
			<Icon name="chevron" size={16} />
		</a>
	{/if}

	{#if status.warnings.length}
		<div class="notice warn">
			{#each status.warnings as w (w)}<div>{w}</div>{/each}
		</div>
	{/if}

	<section aria-labelledby="apps-h">
		<div class="section-head">
			<h2 id="apps-h">Apps</h2>
			<a class="muted small" href="/apps">Manage all</a>
		</div>
		{#if tiles.length === 0}
			<div class="card muted">No apps with a web page yet.</div>
		{:else}
			<div class="tiles">
				{#each tiles as app (app.id)}
					{@const url = openUrl(app, session.hostname)}
					{@const st = stateLabel(app)}
					<div class="tile card" class:stopped={app.state === 'stopped'}>
						<a
							class="open"
							href={url ?? `/apps/${encodeURIComponent(app.id)}`}
							target={url ? '_blank' : undefined}
							rel={url ? 'noopener noreferrer' : undefined}
							title={url ? `Open ${app.title}` : app.title}
							aria-label={url ? `Open ${app.title} (${st.text.toLowerCase()})` : `${app.title} (${st.text.toLowerCase()})`}
						>
							<span class="icon-wrap">
								<AppIcon {app} size={56} />
								<span class="dot {st.tone}" title={st.text}></span>
							</span>
							<span class="name">{app.title}</span>
						</a>
						<a class="more" href={`/apps/${encodeURIComponent(app.id)}`} aria-label="{app.title} details" title="Details">
							<Icon name="more" size={18} />
						</a>
						{#if updateCount(app) > 0}<span class="upd" title="Update available"></span>{/if}
					</div>
				{/each}
			</div>
		{/if}
	</section>

	{#if others.length}
		<section aria-labelledby="other-h">
			<div class="section-head">
				<h2 id="other-h">Other apps and services</h2>
			</div>
			<div class="card list">
				{#each others as app (app.id)}
					{@const st = stateLabel(app)}
					<a class="item" href={`/apps/${encodeURIComponent(app.id)}`}>
						<AppIcon {app} size={30} />
						<span class="grow">
							<span class="title">{app.title}</span>
							<span class="muted small">{kindLabel(app)}{app.memory_bytes ? ` · ${bytes(app.memory_bytes)}` : ''}</span>
						</span>
						<span class="badge {st.tone}">{st.text}</span>
						<Icon name="chevron" size={16} />
					</a>
				{/each}
			</div>
		</section>
	{/if}
{/if}

<style>
	section {
		margin-bottom: 1.6rem;
	}
	.widgets {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(210px, 1fr));
		gap: 0.85rem;
		margin-bottom: 1.6rem;
	}
	.skeleton {
		height: 86px;
		background: linear-gradient(90deg, var(--bg-raised), var(--bg-sunken), var(--bg-raised));
		background-size: 200% 100%;
		animation: shimmer 1.2s infinite;
	}
	@keyframes shimmer {
		to {
			background-position: -200% 0;
		}
	}
	.summary {
		color: inherit;
		text-decoration: none;
		display: flex;
		flex-direction: column;
		justify-content: center;
		padding: 0.85rem 1rem;
	}
	.summary .label {
		display: flex;
		align-items: center;
		gap: 0.3rem;
		color: var(--text-muted);
		font-size: 0.78rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}
	.summary .value {
		font-size: 1.35rem;
		font-weight: 700;
	}
	.summary .value .muted {
		font-size: 0.85rem;
		font-weight: 500;
	}
	.summary .detail {
		font-size: 0.8rem;
	}
	.sysline {
		display: flex;
		align-items: center;
		gap: 0.7rem;
		padding: 0.75rem 1rem;
		margin: -0.6rem 0 1.4rem;
		color: var(--text);
		text-decoration: none;
	}
	.sysline.warn {
		border-color: color-mix(in srgb, var(--warn) 45%, var(--border));
	}
	.sysline .grow {
		flex: 1;
		display: flex;
		gap: 0.5rem;
		align-items: center;
		flex-wrap: wrap;
	}
	.section-head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		margin-bottom: 0.7rem;
	}
	.tiles {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(124px, 1fr));
		gap: 0.85rem;
	}
	.tile {
		position: relative;
		padding: 0;
		transition:
			transform 0.12s,
			box-shadow 0.12s;
	}
	.tile:hover {
		transform: translateY(-2px);
	}
	.tile.stopped .icon-wrap {
		filter: grayscale(1);
		opacity: 0.6;
	}
	.open {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.55rem;
		padding: 1.1rem 0.6rem 0.9rem;
		color: var(--text);
		text-decoration: none;
		border-radius: var(--radius);
	}
	.icon-wrap {
		position: relative;
	}
	.dot {
		position: absolute;
		right: -3px;
		bottom: -3px;
		width: 14px;
		height: 14px;
		border-radius: 50%;
		background: var(--text-muted);
		border: 2.5px solid var(--bg-raised);
	}
	.dot.ok {
		background: var(--ok);
	}
	.dot.warn {
		background: var(--warn);
	}
	.dot.bad {
		background: var(--bad);
	}
	.name {
		font-weight: 600;
		font-size: 0.88rem;
		text-align: center;
		line-height: 1.25;
		overflow: hidden;
		text-overflow: ellipsis;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
	}
	.more {
		position: absolute;
		top: 4px;
		right: 4px;
		padding: 4px;
		border-radius: 8px;
		color: var(--text-muted);
		display: grid;
		place-items: center;
	}
	.more:hover {
		background: var(--bg-sunken);
		color: var(--text);
	}
	.upd {
		position: absolute;
		top: 10px;
		left: 10px;
		width: 9px;
		height: 9px;
		border-radius: 50%;
		background: var(--accent);
	}
	.list {
		padding: 0.3rem;
	}
	.item {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.6rem 0.65rem;
		border-radius: var(--radius-sm);
		color: var(--text);
		text-decoration: none;
	}
	.item:hover {
		background: var(--bg-sunken);
	}
	.item .grow {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.item .title {
		font-weight: 600;
	}
	@media (max-width: 560px) {
		.widgets {
			grid-template-columns: 1fr 1fr;
			gap: 0.6rem;
		}
		.widgets :global(.meter) {
			flex-direction: column;
			align-items: flex-start;
			gap: 0.4rem;
			padding: 0.75rem;
		}
		.widgets :global(.meter svg.ring) {
			width: 40px;
			height: 40px;
		}
		.summary {
			padding: 0.75rem;
		}
	}
	@media (max-width: 420px) {
		.tiles {
			grid-template-columns: repeat(3, 1fr);
			gap: 0.6rem;
		}
		.open {
			padding: 0.9rem 0.3rem 0.7rem;
		}
	}
</style>
