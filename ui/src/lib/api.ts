// Talks to the Hearth API. Every response is {ok, schema, data} or {ok:false, error:{kind,message,exit_code}}.

let csrf = '';

export function setCsrf(token: string) {
	csrf = token;
}

export class ApiError extends Error {
	constructor(
		message: string,
		public kind: string,
		public status: number,
		public exitCode: number
	) {
		super(message);
	}
	get notSignedIn() {
		return this.status === 403 && /not signed in|needs the 'read' scope/.test(this.message);
	}
}

type Opts = { method?: string; body?: unknown; signal?: AbortSignal };

let onSignedOut: (() => void) | null = null;
export function whenSignedOut(f: () => void) {
	onSignedOut = f;
}

export async function api<T>(path: string, opts: Opts = {}): Promise<T> {
	const method = opts.method ?? (opts.body === undefined ? 'GET' : 'POST');
	const headers: Record<string, string> = {};
	if (opts.body !== undefined) headers['content-type'] = 'application/json';
	if (method !== 'GET' && method !== 'HEAD') headers['x-hearth-csrf'] = csrf;
	let res: Response;
	try {
		res = await fetch(path, {
			method,
			headers,
			body: opts.body === undefined ? undefined : JSON.stringify(opts.body),
			credentials: 'same-origin',
			signal: opts.signal
		});
	} catch (e) {
		if ((e as Error).name === 'AbortError') throw e;
		throw new ApiError("Can't reach Hearth. Is the server on?", 'network', 0, 1);
	}
	let j: { ok: boolean; data?: T; error?: { kind: string; message: string; exit_code: number } };
	try {
		j = await res.json();
	} catch {
		throw new ApiError(`Unexpected answer from Hearth (HTTP ${res.status})`, 'other', res.status, 1);
	}
	if (!j.ok) {
		const msg = (j.error?.message ?? 'error').replace(/^not authorized: /, '');
		const err = new ApiError(msg, j.error?.kind ?? 'other', res.status, j.error?.exit_code ?? 1);
		if (err.notSignedIn && onSignedOut) onSignedOut();
		throw err;
	}
	return j.data as T;
}

export const get = <T>(path: string, signal?: AbortSignal) => api<T>(path, { signal });
export const post = <T>(path: string, body: unknown = {}) => api<T>(path, { method: 'POST', body });
export const put = <T>(path: string, body: unknown) => api<T>(path, { method: 'PUT', body });
export const del = <T>(path: string) => api<T>(path, { method: 'DELETE' });

export function errorText(e: unknown): string {
	if (e instanceof ApiError) return e.message;
	if (e instanceof Error) return e.message;
	return String(e);
}

// ----- jobs -----

export type JobStatus = 'running' | 'succeeded' | 'failed' | 'rolled_back' | 'rollback_failed';
export interface Job {
	id: string;
	kind: string;
	app: string;
	status: JobStatus;
	started_at: number;
	finished_at: number | null;
	events: string[];
	events_from: number;
	result: unknown;
	error: string | null;
}

/** Follows a job until it ends, calling onLine for each progress line. */
export async function followJob(id: string, onLine: (l: string) => void): Promise<Job> {
	let after = 0;
	for (;;) {
		const j = await get<Job>(`/v1/jobs/${encodeURIComponent(id)}?after=${after}`);
		for (const l of j.events) onLine(l);
		after = j.events_from + j.events.length;
		if (j.status !== 'running') return j;
		await new Promise((r) => setTimeout(r, 700));
	}
}
