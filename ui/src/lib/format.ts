import type { App } from './types';

export function bytes(n: number | null | undefined): string {
	if (n == null) return '–';
	const units = ['B', 'KB', 'MB', 'GB', 'TB'];
	let i = 0;
	let v = n;
	while (v >= 1024 && i < units.length - 1) {
		v /= 1024;
		i++;
	}
	return `${v >= 100 || i === 0 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

export function percent(part: number, whole: number): number {
	return whole > 0 ? Math.min(100, Math.max(0, (part / whole) * 100)) : 0;
}

export function duration(secs: number): string {
	const d = Math.floor(secs / 86400);
	const h = Math.floor((secs % 86400) / 3600);
	const m = Math.floor((secs % 3600) / 60);
	if (d > 0) return `${d}d ${h}h`;
	if (h > 0) return `${h}h ${m}m`;
	return `${m}m`;
}

export function ago(unix: number | null | undefined): string {
	if (!unix) return 'never';
	const s = Math.max(0, Date.now() / 1000 - unix);
	if (s < 60) return 'just now';
	if (s < 3600) return `${Math.floor(s / 60)} min ago`;
	if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
	if (s < 86400 * 30) return `${Math.floor(s / 86400)} days ago`;
	return new Date(unix * 1000).toLocaleDateString();
}

export function dateTime(unix: number): string {
	return new Date(unix * 1000).toLocaleString(undefined, {
		dateStyle: 'medium',
		timeStyle: 'short'
	});
}

/** The app's web address as seen from this browser: "http://jarvis:8123" becomes the host
 * the browser used to reach Hearth (so it works over Tailscale and from phones too). */
export function openUrl(app: App, serverHostname: string): string | null {
	if (!app.web_url) return null;
	try {
		const u = new URL(app.web_url);
		const h = u.hostname.toLowerCase();
		const local = [serverHostname.toLowerCase(), 'localhost', '127.0.0.1', '0.0.0.0', '[::]'];
		if (local.includes(h) || h === `${serverHostname.toLowerCase()}.local`) {
			u.hostname = location.hostname;
		}
		return u.toString();
	} catch {
		return app.web_url;
	}
}

export function updateCount(app: App): number {
	return app.images.filter((i) => i.latest?.update_available).length;
}

export function stateLabel(app: App): { text: string; tone: 'ok' | 'warn' | 'bad' | '' } {
	if (app.health === 'unhealthy') return { text: 'Unhealthy', tone: 'bad' };
	if (app.health === 'starting') return { text: 'Starting', tone: 'warn' };
	switch (app.state) {
		case 'running':
			return { text: 'Running', tone: 'ok' };
		case 'partial':
			return { text: 'Partly running', tone: 'warn' };
		case 'stopped':
			return { text: 'Stopped', tone: '' };
		default:
			return { text: 'Not settled', tone: 'bad' };
	}
}

export function kindLabel(app: App): string {
	switch (app.kind) {
		case 'compose':
			return 'Docker';
		case 'service':
			return 'Service';
		case 'native':
			return 'Program';
		case 'link':
			return 'Link';
		case 'package':
			return 'Package';
	}
}
