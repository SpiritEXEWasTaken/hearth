<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { get, post, errorText } from '#lib/api.ts';
	import type { StoreEntryView, InstallPlan, HostFacts, StoreOption } from '#lib/types.ts';
	import { badges, sizeText } from '#lib/store.ts';
	import { session } from '#lib/session.svelte.ts';
	import { JobRun } from '#lib/components/jobrun.svelte.ts';
	import Icon from '#lib/components/Icon.svelte';
	import Modal from '#lib/components/Modal.svelte';
	import JobPanel from '#lib/components/JobPanel.svelte';

	const id = $derived(page.params.id ?? '');
	let view = $state<StoreEntryView | null>(null);
	let host = $state<HostFacts | null>(null);
	let error = $state('');
	let plan = $state<InstallPlan | null>(null);
	let planning = $state(false);
	let planError = $state('');

	// The install form.
	let name = $state('');
	let answers = $state<Record<string, string>>({});
	let ports = $state<Record<string, number>>({});
	let https = $state(false);
	let showCompose = $state(false);
	const job = new JobRun();

	async function load() {
		try {
			const [v, list] = await Promise.all([
				get<StoreEntryView>(`/v1/store/apps/${encodeURIComponent(id)}`),
				get<{ host: HostFacts }>('/v1/store')
			]);
			view = v;
			host = list.host;
			plan = v.plan;
			name = v.plan.name;
			answers = { ...v.plan.answers };
			ports = Object.fromEntries(v.plan.ports.filter((p) => p.env).map((p) => [p.env as string, p.port]));
			https = v.plan.https;
			error = '';
		} catch (e) {
			error = errorText(e);
		}
	}
	onMount(load);

	let timer: ReturnType<typeof setTimeout> | undefined;
	let seq = 0;
	function replan() {
		clearTimeout(timer);
		timer = setTimeout(async () => {
			const mine = ++seq;
			planning = true;
			try {
				const p = await post<InstallPlan>(`/v1/store/apps/${encodeURIComponent(id)}/plan`, {
					name,
					options: answers,
					ports,
					https
				});
				if (mine === seq) {
					plan = p;
					planError = '';
				}
			} catch (e) {
				if (mine === seq) planError = errorText(e);
			} finally {
				if (mine === seq) planning = false;
			}
		}, 300);
	}

	function unavailable(o: StoreOption, choice: string): string | undefined {
		return plan?.options.find((x) => x.id === o.id)?.unavailable[choice];
	}

	async function install() {
		if (!plan || !view) return;
		const done = await job.run(`Installing ${view.entry.name}`, () =>
			post<{ job: string }>(`/v1/store/apps/${encodeURIComponent(id)}/install`, {
				name: plan!.name,
				options: answers,
				ports: Object.fromEntries(plan!.ports.filter((p) => p.env).map((p) => [p.env as string, p.port])),
				https
			})
		);
		if (done?.status === 'succeeded') {
			job.open = false;
			await goto(`/apps/${encodeURIComponent(plan.name)}?installed=1`);
		}
	}

	const checkIcon = { ok: 'check', info: 'info', warn: 'alert', error: 'alert' } as const;
	const errors = $derived(plan?.checks.filter((c) => c.level === 'error').length ?? 0);
</script>

<svelte:head><title>{view?.entry.name ?? 'Store'} · Hearth</title></svelte:head>

<a class="back muted small" href="/store"><Icon name="back" size={16} /> Store</a>

{#if error && !view}
	<div class="notice bad">{error}</div>
{:else if !view}
	<div class="card muted">Loading…</div>
{:else}
	{@const e = view.entry}
	<header class="head">
		<img src={`/v1/store/icons/${encodeURIComponent(e.id)}`} alt="" width="72" height="72" />
		<div class="title">
			<h1>{e.name}</h1>
			<p class="muted">{e.summary}</p>
			<div class="badges">
				{#each badges(view.app.badges, host ?? undefined) as b (b.kind)}
					<span class="badge {b.tone ?? ''}" title={b.title}>{b.text}</span>
				{/each}
			</div>
		</div>
	</header>

	{#if view.installed.length}
		<div class="notice ok">
			Installed here as
			{#each view.installed as n, i (n)}{i ? ', ' : ''}<a href={`/apps/${encodeURIComponent(n)}`}>{n}</a>{/each}.
		</div>
	{/if}
	{#if e.notice}
		<div class="notice warn" data-testid="content-notice"><Icon name="info" size={16} /> {e.notice}</div>
	{/if}

	{#if e.screenshots?.length}
		<div class="shots" aria-label="Screenshots">
			{#each e.screenshots ?? [] as s (s.file)}
				<figure>
					<img src={`/v1/store/files/${encodeURIComponent(e.id)}/${s.file}`} alt={s.caption} loading="lazy" />
					<figcaption class="muted small">{s.caption}</figcaption>
				</figure>
			{/each}
		</div>
	{/if}

	<div class="cols">
		<section class="stack">
			<div class="card stack">
				{#each e.description.split(/\n\s*\n/) as p, i (i)}
					<p>{p}</p>
				{/each}
			</div>
			<div class="card">
				<h2>Details</h2>
				<dl>
					<dt>Website</dt>
					<dd><a href={e.upstream.website} target="_blank" rel="noopener noreferrer">{e.upstream.website}</a></dd>
					<dt>Source code</dt>
					<dd><a href={e.upstream.source} target="_blank" rel="noopener noreferrer">{e.upstream.source}</a></dd>
					{#if e.upstream.docs}
						<dt>Docs</dt>
						<dd><a href={e.upstream.docs} target="_blank" rel="noopener noreferrer">{e.upstream.docs}</a></dd>
					{/if}
					<dt>Licence</dt>
					<dd>{e.license}</dd>
					<dt>Store id</dt>
					<dd class="mono small">{e.id}</dd>
				</dl>
			</div>
		</section>

		<section class="card stack install" aria-label="Install">
			<h2>Install</h2>
			{#if !session.isAdmin}
				<p class="muted small">Only admins can install apps. Here's how it would fit this server:</p>
			{/if}
			<label>
				Name on this server
				<input bind:value={name} oninput={replan} disabled={!session.isAdmin} autocomplete="off" />
			</label>

			{#each e.options ?? [] as o (o.id)}
				{#if o.kind === 'switch'}
					<label class="check">
						<input
							type="checkbox"
							checked={answers[o.id] === 'true'}
							disabled={!session.isAdmin}
							onchange={(ev) => {
								answers[o.id] = (ev.currentTarget as HTMLInputElement).checked ? 'true' : 'false';
								replan();
							}}
						/>
						<span>
							{o.label}
							{#if o.help}<span class="hint">{o.help}</span>{/if}
						</span>
					</label>
				{:else if o.kind === 'choice' || o.kind === 'gpu'}
					<fieldset>
						<legend>{o.label}</legend>
						{#if o.help}<p class="hint">{o.help}</p>{/if}
						{#each o.choices ?? [] as c (c.id)}
							{@const why = unavailable(o, c.id)}
							<label class="check" class:dim={why}>
								<input
									type="radio"
									name={`opt-${o.id}`}
									value={c.id}
									checked={answers[o.id] === c.id}
									disabled={!session.isAdmin || !!why}
									onchange={() => {
										answers[o.id] = c.id;
										replan();
									}}
								/>
								<span>{c.label}{#if why}<span class="hint">Not on this server: {why}</span>{/if}</span>
							</label>
						{/each}
					</fieldset>
				{:else}
					<label>
						{o.label}
						{#if o.help}<span class="hint">{o.help}</span>{/if}
						<input
							value={(answers[o.id] ?? '').replace('${APP_DATA}', plan?.data_dir ?? '')}
							disabled={!session.isAdmin}
							oninput={(ev) => {
								const v = (ev.currentTarget as HTMLInputElement).value;
								const dd = plan?.data_dir ?? '';
								answers[o.id] = dd && v.startsWith(dd) ? '${APP_DATA}' + v.slice(dd.length) : v;
								replan();
							}}
						/>
					</label>
				{/if}
			{/each}

			{#if plan && plan.ports.some((p) => p.env && p.movable)}
				<fieldset>
					<legend>Ports</legend>
					{#each plan.ports.filter((p, i, all) => p.env && p.movable && all.findIndex((q) => q.env === p.env) === i) as p (p.env)}
						<label class="port">
							<span>{p.label}</span>
							<input
								type="number"
								min="1024"
								max="65535"
								value={ports[p.env as string] ?? p.port}
								disabled={!session.isAdmin}
								oninput={(ev) => {
									const n = Number((ev.currentTarget as HTMLInputElement).value);
									if (n > 0) ports[p.env as string] = n;
									replan();
								}}
							/>
						</label>
					{/each}
				</fieldset>
			{/if}

			{#if e.web && e.web.https !== 'no'}
				<label class="check">
					<input type="checkbox" bind:checked={https} onchange={replan} disabled={!session.isAdmin} />
					<span>
						Turn on HTTPS for this app
						<span class="hint">
							{e.web.https === 'required'
								? 'Needed: the app and its phone apps only work over HTTPS.'
								: 'Recommended for phone apps and camera access. You can also do it later.'}
						</span>
					</span>
				</label>
			{/if}

			<div class="checks" aria-live="polite" data-testid="host-checks">
				<h3>
					Checks for this server
					{#if planning}<span class="muted small">checking…</span>{/if}
				</h3>
				{#if planError}<div class="notice bad">{planError}</div>{/if}
				{#each plan?.checks ?? [] as c, i (i)}
					<div class="chk {c.level}">
						<Icon name={checkIcon[c.level]} size={16} />
						<span>{c.message}</span>
					</div>
				{/each}
			</div>

			{#if session.isAdmin}
				<button
					class="primary"
					onclick={install}
					disabled={!plan || !plan.ok || planning || job.active}
					data-testid="install-button"
				>
					<Icon name="download" size={18} />
					{errors ? `Can't install (${errors} problem${errors > 1 ? 's' : ''})` : `Install ${e.name}`}
				</button>
				{#if plan}
					<p class="muted small">
						Downloads about {sizeText(plan.download_mb)}. Data goes to <span class="mono">{plan.data_dir}</span>.
					</p>
					<button class="ghost small" onclick={() => (showCompose = !showCompose)}>
						{showCompose ? 'Hide' : 'Show'} the setup file (advanced)
					</button>
					{#if showCompose}<pre class="log">{plan.compose}</pre>{/if}
				{/if}
			{/if}
		</section>
	</div>
{/if}

<Modal title={job.title || 'Installing'} bind:open={job.open} wide>
	<JobPanel title="" lines={job.lines} job={job.job} error={job.error} />
	{#snippet actions()}
		<button class="primary" onclick={() => (job.open = false)} disabled={job.active}>{job.active ? 'Working…' : 'Close'}</button>
	{/snippet}
</Modal>

<style>
	.back {
		display: inline-flex;
		align-items: center;
		gap: 0.2rem;
		text-decoration: none;
		margin-bottom: 0.8rem;
	}
	.head {
		display: flex;
		gap: 1rem;
		align-items: center;
		margin-bottom: 1rem;
	}
	.head img {
		object-fit: contain;
		border-radius: 22%;
		flex: none;
	}
	.title {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		min-width: 0;
	}
	.title p {
		margin: 0;
	}
	.badges {
		display: flex;
		gap: 0.3rem;
		flex-wrap: wrap;
	}
	.notice {
		margin-bottom: 1rem;
	}
	.shots {
		display: flex;
		gap: 0.85rem;
		overflow-x: auto;
		scroll-snap-type: x mandatory;
		padding-bottom: 0.5rem;
		margin-bottom: 1rem;
	}
	.shots figure {
		margin: 0;
		flex: 0 0 min(88%, 620px);
		scroll-snap-align: start;
	}
	.shots img {
		width: 100%;
		border-radius: var(--radius-sm);
		border: 1px solid var(--border);
		display: block;
	}
	.cols {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(300px, 400px);
		gap: 1rem;
		align-items: start;
	}
	@media (max-width: 860px) {
		.cols {
			grid-template-columns: 1fr;
		}
	}
	.card p {
		margin: 0;
	}
	dl {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.35rem 1rem;
		margin: 0.5rem 0 0;
		overflow-wrap: anywhere;
	}
	dt {
		color: var(--text-muted);
	}
	dd {
		margin: 0;
	}
	fieldset {
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
		padding: 0.5rem 0.75rem 0.65rem;
		margin: 0;
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}
	legend {
		font-weight: 600;
		padding: 0 0.3rem;
	}
	.hint {
		display: block;
		font-weight: 400;
		color: var(--text-muted);
		font-size: 0.8rem;
		margin: 0;
	}
	label.check {
		display: flex;
		flex-direction: row;
		gap: 0.55rem;
		align-items: flex-start;
		font-weight: 500;
	}
	label.check input {
		width: auto;
		margin-top: 0.2rem;
		flex: none;
	}
	label.dim {
		opacity: 0.55;
	}
	label.port {
		display: flex;
		flex-direction: row;
		align-items: center;
		gap: 0.6rem;
		justify-content: space-between;
		font-weight: 500;
	}
	label.port input {
		width: 7rem;
	}
	.checks h3 {
		font-size: 0.95rem;
		margin: 0.3rem 0 0.4rem;
		display: flex;
		gap: 0.5rem;
		align-items: baseline;
	}
	.chk {
		display: flex;
		gap: 0.45rem;
		align-items: flex-start;
		font-size: 0.88rem;
		padding: 0.2rem 0;
	}
	.chk :global(svg) {
		flex: none;
		margin-top: 0.15rem;
	}
	.chk.ok :global(svg) {
		color: var(--ok);
	}
	.chk.info :global(svg) {
		color: var(--info);
	}
	.chk.warn :global(svg) {
		color: var(--warn);
	}
	.chk.error {
		color: var(--bad);
	}
	.log {
		max-height: 320px;
		font-size: 0.78rem;
	}
</style>
