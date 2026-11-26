<script lang="ts">
	import type { AppDetail } from '#lib/types.ts';
	import type { JobRun } from '#lib/components/jobrun.svelte.ts';
	import { bytes } from '#lib/format.ts';
	import HttpsCard from './HttpsCard.svelte';
	import StoreCard from './StoreCard.svelte';
	import { page } from '$app/state';
	let {
		app,
		canChange,
		job,
		onchange
	}: { app: AppDetail; canChange: boolean; job: JobRun; onchange: () => void } = $props();

	function shortDigest(d: string | null | undefined) {
		return d ? d.replace(/^sha256:/, '').slice(0, 12) : '–';
	}
</script>

<div class="stack">
	{#if app.missing_devices.length}
		<div class="notice warn">
			These devices are in the app's setup but don't exist on this server:
			<span class="mono">{app.missing_devices.join(', ')}</span>
		</div>
	{/if}
	{#if app.external_variables.length && app.managed_by !== 'hearth'}
		<div class="notice">
			This compose file needs values from outside (<span class="mono">{app.external_variables.join(', ')}</span>),
			so it only runs through its manager.
		</div>
	{/if}

	<div class="grid">
		<div class="card stack">
			<h2>About</h2>
			<dl>
				<dt>Memory</dt>
				<dd>{bytes(app.memory_bytes)}</dd>
				{#if app.health}
					<dt>Health check</dt>
					<dd>{app.health}</dd>
				{/if}
				<dt>Managed by</dt>
				<dd>
					{{ hearth: 'Hearth', casaos: 'CasaOS', compose: 'Docker Compose (outside Hearth)', container: 'Docker (single container)' }[
						app.managed_by
					]}
				</dd>
				{#if app.endpoint}
					<dt>Docker</dt>
					<dd>{app.endpoint.kind === 'rootless' ? `rootless (user ${app.endpoint.user ?? ''})` : 'system'}</dd>
				{/if}
				{#if app.web_url}
					<dt>Address</dt>
					<dd class="mono">{app.web_url}</dd>
				{/if}
				{#if app.working_dir}
					<dt>Folder</dt>
					<dd class="mono">{app.working_dir}</dd>
				{/if}
			</dl>
		</div>

		{#if app.kind === 'compose'}
			<StoreCard {app} justInstalled={page.url.searchParams.has('installed')} />
		{/if}

		{#if app.web_url}
			<HttpsCard {app} {canChange} {job} {onchange} />
		{/if}

		{#if app.unit}
			<div class="card stack">
				<h2>Service</h2>
				<dl>
					<dt>Unit</dt>
					<dd class="mono">{app.unit.name}{app.unit.user ? ` (user ${app.unit.user})` : ''}</dd>
					<dt>State</dt>
					<dd>{app.unit.active_state} ({app.unit.sub_state})</dd>
					<dt>Starts at boot</dt>
					<dd>{app.unit.unit_file_state}</dd>
				</dl>
			</div>
		{/if}

		{#if app.images.length}
			<div class="card stack">
				<h2>Versions</h2>
				<div class="table-wrap">
					<table>
						<thead><tr><th>Part</th><th>Running</th><th>Latest</th></tr></thead>
						<tbody>
							{#each app.images as img (img.service + img.reference)}
								<tr>
									<td>
										<div>{img.service}</div>
										<div class="muted small mono">{img.reference}</div>
									</td>
									<td>{img.running_version ?? shortDigest(img.running_digest)}</td>
									<td>
										{#if !img.latest}
											<span class="muted">not checked</span>
										{:else if img.latest.error}
											<span class="muted" title={img.latest.error}>couldn't check</span>
										{:else if img.latest.update_available}
											<span class="badge accent">{img.latest.version ?? 'newer build'}</span>
										{:else}
											<span class="badge ok">up to date</span>
										{/if}
										{#if img.latest?.newer_major}
											<div class="muted small">New major version: {img.latest.newer_major}</div>
										{/if}
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			</div>
		{/if}
	</div>

	{#if app.containers.length}
		<div class="card stack">
			<h2>Containers</h2>
			<div class="table-wrap">
				<table>
					<thead><tr><th>Name</th><th>Status</th><th>Memory</th><th>Restarts</th></tr></thead>
					<tbody>
						{#each app.containers as c (c.id)}
							<tr>
								<td>
									<div>{c.service ?? c.name}</div>
									<div class="muted small mono">{c.image}</div>
								</td>
								<td>
									<span class="badge {c.state === 'running' ? (c.health === 'unhealthy' ? 'bad' : 'ok') : c.state === 'exited' ? '' : 'warn'}"
										>{c.state}</span
									>
									<div class="muted small">{c.status}</div>
								</td>
								<td>{bytes(c.memory_bytes)}</td>
								<td>{c.restart_count ?? '–'}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		</div>
	{/if}
</div>

<style>
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
		gap: 1rem;
	}
	dl {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.4rem 1rem;
		margin: 0;
		font-size: 0.9rem;
	}
	dt {
		color: var(--text-muted);
	}
	dd {
		margin: 0;
		word-break: break-word;
	}
</style>
