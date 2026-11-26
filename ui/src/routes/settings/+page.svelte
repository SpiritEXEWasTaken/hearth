<script lang="ts">
	import { onMount } from 'svelte';
	import { get, post, put, del, errorText } from '#lib/api.ts';
	import { session } from '#lib/session.svelte.ts';
	import type { User, TlsInfo, TokenInfo } from '#lib/types.ts';
	import { ago } from '#lib/format.ts';
	import Modal from '#lib/components/Modal.svelte';
	import Icon from '#lib/components/Icon.svelte';

	// ----- my account -----
	let oldPw = $state('');
	let newPw = $state('');
	let newPw2 = $state('');
	let pwMsg = $state<{ tone: string; text: string } | null>(null);

	async function changePassword(e: SubmitEvent) {
		e.preventDefault();
		pwMsg = null;
		if (newPw !== newPw2) {
			pwMsg = { tone: 'bad', text: "The new passwords don't match." };
			return;
		}
		try {
			await post('/v1/auth/password', { old_password: oldPw, new_password: newPw });
			pwMsg = { tone: 'ok', text: 'Password changed. Other devices were signed out.' };
			oldPw = newPw = newPw2 = '';
		} catch (err) {
			pwMsg = { tone: 'bad', text: errorText(err) };
		}
	}

	// ----- users -----
	let users = $state<User[]>([]);
	let usersError = $state('');
	let addOpen = $state(false);
	let nu = $state({ username: '', password: '', role: 'viewer' });
	let addError = $state('');
	let pwUser = $state<User | null>(null);
	let pwOpen = $state(false);
	let resetPw = $state('');
	let resetError = $state('');

	async function loadUsers() {
		if (!session.isAdmin) return;
		try {
			users = await get<User[]>('/v1/users');
		} catch (e) {
			usersError = errorText(e);
		}
	}

	async function addUser(e: SubmitEvent) {
		e.preventDefault();
		addError = '';
		try {
			await post('/v1/users', nu);
			addOpen = false;
			nu = { username: '', password: '', role: 'viewer' };
			await loadUsers();
		} catch (err) {
			addError = errorText(err);
		}
	}

	async function setRole(u: User, role: string) {
		usersError = '';
		try {
			await put(`/v1/users/${u.id}`, { role });
		} catch (e) {
			usersError = errorText(e);
		}
		await loadUsers();
	}

	async function removeUser(u: User) {
		usersError = '';
		if (!confirmRemove(u)) return;
		try {
			await del(`/v1/users/${u.id}`);
		} catch (e) {
			usersError = errorText(e);
		}
		await loadUsers();
	}

	// A typed confirmation instead of a browser dialog.
	let removeTarget = $state<User | null>(null);
	let removeOpen = $state(false);
	function confirmRemove(u: User) {
		if (removeTarget?.id === u.id) {
			removeTarget = null;
			return true;
		}
		removeTarget = u;
		removeOpen = true;
		return false;
	}

	async function resetPassword(e: SubmitEvent) {
		e.preventDefault();
		resetError = '';
		if (!pwUser) return;
		try {
			await put(`/v1/users/${pwUser.id}`, { password: resetPw });
			pwOpen = false;
			resetPw = '';
		} catch (err) {
			resetError = errorText(err);
		}
	}

	// ----- HTTPS -----
	let tls = $state<TlsInfo | null>(null);

	// ----- tokens -----
	let tokens = $state<TokenInfo[]>([]);
	let tokenError = $state('');
	let tokOpen = $state(false);
	let tokName = $state('');
	let tokScopes = $state<string[]>(['read']);
	let newSecret = $state('');

	async function loadTokens() {
		if (!session.isAdmin) return;
		try {
			tokens = await get<TokenInfo[]>('/v1/tokens');
		} catch (e) {
			tokenError = errorText(e);
		}
	}

	async function createToken(e: SubmitEvent) {
		e.preventDefault();
		tokenError = '';
		try {
			const t = await post<{ token: string }>('/v1/tokens', { name: tokName, scopes: tokScopes });
			newSecret = t.token;
			tokName = '';
			await loadTokens();
		} catch (err) {
			tokenError = errorText(err);
		}
	}

	async function revoke(t: TokenInfo) {
		try {
			await del(`/v1/tokens/${t.id}`);
		} catch (e) {
			tokenError = errorText(e);
		}
		await loadTokens();
	}

	let version = $state('');

	onMount(async () => {
		loadUsers();
		loadTokens();
		get<TlsInfo>('/v1/tls').then((t) => (tls = t)).catch(() => {});
		get<{ version: string }>('/v1/ping').then((p) => (version = p.version)).catch(() => {});
	});

	const scopeInfo: [string, string][] = [
		['read', 'See everything'],
		['apps:write', 'Change apps (start, stop, update, edit)'],
		['system:write', 'System updates and packages'],
		['admin', 'Everything, including users and tokens']
	];

	function hostOf(n: string) {
		return n.includes(':') ? `[${n}]` : n;
	}
</script>

<svelte:head><title>Settings · Hearth</title></svelte:head>

<h1 class="page-title">Settings</h1>

<div class="sections">
	<section class="card stack" id="account">
		<h2><Icon name="user" size={18} /> Your account</h2>
		<div class="muted small">
			Signed in as <strong>{session.me?.user.username}</strong> ({session.isAdmin ? 'admin' : 'read-only'}).
		</div>
		<form class="stack" onsubmit={changePassword}>
			<input type="text" autocomplete="username" value={session.me?.user.username} hidden readonly />
			<label>Current password<input type="password" bind:value={oldPw} autocomplete="current-password" required /></label>
			<label>New password<input type="password" bind:value={newPw} autocomplete="new-password" minlength="8" required /></label>
			<label>New password again<input type="password" bind:value={newPw2} autocomplete="new-password" minlength="8" required /></label>
			{#if pwMsg}<div class="notice {pwMsg.tone}">{pwMsg.text}</div>{/if}
			<div><button class="primary" type="submit">Change password</button></div>
		</form>
	</section>

	{#if session.isAdmin}
		<section class="card stack" id="users">
			<div class="row">
				<h2><Icon name="user" size={18} /> Users</h2>
				<div class="spacer"></div>
				<button class="small" onclick={() => (addOpen = true)}><Icon name="plus" size={16} /> Add user</button>
			</div>
			<p class="muted small">Admins can change everything. Read-only users can see apps and open them, but not change anything.</p>
			{#if usersError}<div class="notice bad">{usersError}</div>{/if}
			<div class="table-wrap">
				<table>
					<thead><tr><th>User</th><th>Role</th><th>Last sign-in</th><th></th></tr></thead>
					<tbody>
						{#each users as u (u.id)}
							<tr>
								<td><strong>{u.username}</strong>{#if u.id === session.me?.user.id} <span class="muted small">(you)</span>{/if}</td>
								<td>
									<select
										value={u.role}
										onchange={(e) => setRole(u, (e.currentTarget as HTMLSelectElement).value)}
										aria-label="Role of {u.username}"
									>
										<option value="admin">Admin</option>
										<option value="viewer">Read-only</option>
									</select>
								</td>
								<td class="muted small">{ago(u.last_login_at)}</td>
								<td class="right nowrap">
									<button
										class="small"
										onclick={() => {
											pwUser = u;
											pwOpen = true;
										}}>Set password</button
									>
									{#if u.id !== session.me?.user.id}
										<button class="small danger" onclick={() => removeUser(u)} aria-label="Remove {u.username}"><Icon name="trash" size={15} /></button>
									{/if}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		</section>
	{/if}

	<section class="card stack" id="https">
		<h2><Icon name="lock" size={18} /> HTTPS</h2>
		{#if tls}
			{#if tls.tailscale_names.length}
				<div>
					<div class="small muted">Over Tailscale (trusted on every device, no setup):</div>
					{#each tls.tailscale_names as n (n)}
						<div><a href="https://{n}" class="mono">https://{n}</a></div>
					{/each}
				</div>
			{:else}
				<div class="notice">
					No Tailscale certificate. If this server uses Tailscale, turn on HTTPS certificates in the Tailscale admin
					console (DNS page) and Hearth picks it up within 12 hours (or after a restart).
				</div>
			{/if}
			<div>
				<div class="small muted">At home (Hearth's own certificate):</div>
				<div class="addr">
					{#each tls.local_names.filter((n) => n !== 'localhost' && n !== '127.0.0.1' && n !== '::1') as n (n)}
						<span class="mono">https://{hostOf(n)}</span>
					{/each}
				</div>
			</div>
			<div class="notice">
				Browsers warn about Hearth's own certificate until you install Hearth's certificate authority on that device
				once. <a href="/ca.crt" download>Download it</a>, then open it: on Android it's under Settings → Security →
				Encryption → Install a certificate → CA certificate; on Windows double-click → Install → Local machine →
				"Trusted Root Certification Authorities"; on iPhone install the profile, then turn it on in Settings → General →
				About → Certificate Trust Settings.
				{#if tls.ca_fingerprint_sha256}
					<div class="small muted fp">Fingerprint (SHA-256): <span class="mono">{tls.ca_fingerprint_sha256}</span></div>
				{/if}
			</div>
		{:else}
			<span class="muted">Loading…</span>
		{/if}
	</section>

	{#if session.isAdmin}
		<section class="card stack" id="tokens">
			<div class="row">
				<h2><Icon name="key" size={18} /> API tokens</h2>
				<div class="spacer"></div>
				<button
					class="small"
					onclick={() => {
						newSecret = '';
						tokOpen = true;
					}}><Icon name="plus" size={16} /> New token</button
				>
			</div>
			<p class="muted small">For scripts and AI agents (the <span class="mono">hearth</span> CLI, Claude). Each token only gets the rights you tick.</p>
			{#if tokenError}<div class="notice bad">{tokenError}</div>{/if}
			<div class="table-wrap">
				<table>
					<thead><tr><th>Name</th><th>Rights</th><th>Last used</th><th></th></tr></thead>
					<tbody>
						{#each tokens as t (t.id)}
							<tr>
								<td><strong>{t.name}</strong><div class="muted small mono">{t.id}</div></td>
								<td>{#each t.scopes as s (s)}<span class="badge">{s}</span> {/each}</td>
								<td class="muted small">{ago(t.last_used_at)}</td>
								<td class="right"><button class="small danger" onclick={() => revoke(t)}>Revoke</button></td>
							</tr>
						{:else}
							<tr><td colspan="4" class="muted">No tokens.</td></tr>
						{/each}
					</tbody>
				</table>
			</div>
		</section>
	{/if}

	<section class="card stack">
		<h2><Icon name="info" size={18} /> About</h2>
		<div class="small">Hearth {version} on {session.hostname}</div>
		<div class="small muted"><a href="https://github.com/SpiritEXEWasTaken/hearth" target="_blank" rel="noopener noreferrer">Source code</a> · Apache-2.0 or MIT</div>
	</section>
</div>

<Modal title="Add a user" bind:open={addOpen}>
	<form id="add-user" class="stack" onsubmit={addUser}>
		<label>Username<input bind:value={nu.username} autocomplete="off" autocapitalize="none" required /></label>
		<label>Password<span class="hint">At least 8 characters. They can change it later.</span><input type="password" bind:value={nu.password} autocomplete="new-password" minlength="8" required /></label>
		<label>
			Role
			<select bind:value={nu.role}>
				<option value="viewer">Read-only</option>
				<option value="admin">Admin</option>
			</select>
		</label>
		{#if addError}<div class="notice bad">{addError}</div>{/if}
	</form>
	{#snippet actions()}
		<button onclick={() => (addOpen = false)}>Cancel</button>
		<button class="primary" type="submit" form="add-user">Add</button>
	{/snippet}
</Modal>

<Modal title="Set a new password for {pwUser?.username}" bind:open={pwOpen}>
	<form id="reset-pw" class="stack" onsubmit={resetPassword}>
		<label>New password<input type="password" bind:value={resetPw} autocomplete="new-password" minlength="8" required /></label>
		<div class="muted small">They'll be signed out on their other devices.</div>
		{#if resetError}<div class="notice bad">{resetError}</div>{/if}
	</form>
	{#snippet actions()}
		<button onclick={() => (pwOpen = false)}>Cancel</button>
		<button class="primary" type="submit" form="reset-pw">Save</button>
	{/snippet}
</Modal>

<Modal title="Remove {removeTarget?.username}?" bind:open={removeOpen}>
	<p>They won't be able to sign in any more.</p>
	{#snippet actions()}
		<button
			onclick={() => {
				removeOpen = false;
				removeTarget = null;
			}}>Cancel</button
		>
		<button
			class="danger solid"
			onclick={() => {
				removeOpen = false;
				if (removeTarget) removeUser(removeTarget);
			}}>Remove</button
		>
	{/snippet}
</Modal>

<Modal title="New API token" bind:open={tokOpen}>
	{#if newSecret}
		<div class="notice ok">Copy this now. It won't be shown again.</div>
		<pre class="secret">{newSecret}</pre>
		<div class="muted small">On the server, save it in <span class="mono">~/.config/hearth/token</span> (mode 600).</div>
	{:else}
		<form id="new-token" class="stack" onsubmit={createToken}>
			<label>Name<input bind:value={tokName} placeholder="e.g. claude" required maxlength="64" /></label>
			<fieldset>
				<legend>Rights</legend>
				{#each scopeInfo as [s, label] (s)}
					<label class="check"><input type="checkbox" value={s} bind:group={tokScopes} /> <span><span class="mono">{s}</span> · {label}</span></label>
				{/each}
			</fieldset>
			{#if tokenError}<div class="notice bad">{tokenError}</div>{/if}
		</form>
	{/if}
	{#snippet actions()}
		{#if newSecret}
			<button class="primary" onclick={() => (tokOpen = false)}>Done</button>
		{:else}
			<button onclick={() => (tokOpen = false)}>Cancel</button>
			<button class="primary" type="submit" form="new-token" disabled={!tokScopes.length}>Create</button>
		{/if}
	{/snippet}
</Modal>

<style>
	.page-title {
		margin-bottom: 1rem;
	}
	.sections {
		display: grid;
		gap: 1rem;
		max-width: 820px;
	}
	h2 {
		display: flex;
		align-items: center;
		gap: 0.45rem;
	}
	.right {
		text-align: right;
	}
	.nowrap {
		white-space: nowrap;
	}
	td select {
		width: auto;
		min-height: 32px;
		padding: 0.2rem 0.5rem;
	}
	.addr {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem 1rem;
	}
	.fp {
		margin-top: 0.4rem;
		word-break: break-all;
	}
	.secret {
		user-select: all;
		word-break: break-all;
	}
	fieldset {
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
		padding: 0.5rem 0.8rem 0.7rem;
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
	}
	legend {
		font-weight: 600;
		font-size: 0.88rem;
		padding: 0 0.3rem;
	}
	.check {
		flex-direction: row;
		align-items: center;
		gap: 0.5rem;
		font-weight: 400;
	}
	.check input {
		width: auto;
		min-height: 0;
	}
</style>
