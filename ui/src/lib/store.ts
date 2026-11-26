// Store badge texts (the same words as the store's public site).
import type { StoreBadges } from './types.ts';

export function sizeText(mb: number): string {
	if (mb >= 1024) {
		const gb = mb / 1024;
		return gb >= 10 ? `${gb.toFixed(0)} GB` : `${gb.toFixed(1)} GB`;
	}
	return `${mb} MB`;
}

export type Badge = { kind: string; text: string; tone?: 'warn' | 'bad' | 'accent' | 'info'; title?: string };

/** Badges for a store app, judged against this server when `host` is given. */
export function badges(
	b: StoreBadges,
	host?: { arch: string; mem_total_mb: number; disk_free_mb: number | null; gpus: string[] }
): Badge[] {
	const out: Badge[] = [];
	const ramTone = host && host.mem_total_mb < b.ram_mb ? 'warn' : undefined;
	out.push({
		kind: 'ram',
		text: `RAM ${sizeText(b.ram_mb)}`,
		tone: ramTone,
		title: b.ram_recommended_mb
			? `Needs about ${sizeText(b.ram_mb)}; ${sizeText(b.ram_recommended_mb)} for comfortable use`
			: `Needs about ${sizeText(b.ram_mb)} of RAM`
	});
	const arch = host?.arch ?? 'amd64';
	const dl = b.download_mb?.[arch] ?? Object.values(b.download_mb ?? {})[0] ?? 0;
	const disk = dl + b.data_mb;
	out.push({
		kind: 'disk',
		text: `Disk ~${sizeText(disk)}`,
		tone: host?.disk_free_mb != null && host.disk_free_mb < disk ? 'bad' : undefined,
		title: `${sizeText(dl)} download + about ${sizeText(b.data_mb)} of its own data`
	});
	const fits = !host || b.architectures.includes(host.arch);
	const archText =
		b.architectures.length === 1
			? b.architectures[0] === 'amd64'
				? 'x86 (Intel/AMD) only'
				: b.architectures[0] === 'arm64'
					? 'ARM only'
					: b.architectures[0]
			: 'x86 + ARM';
	out.push({
		kind: 'cpu',
		text: fits ? archText : `Not for this CPU (${archText})`,
		tone: fits ? undefined : 'bad'
	});
	if (b.gpu !== 'none') {
		const text = { optional: 'Can use a GPU', recommended: 'GPU recommended', required: 'Needs a GPU' }[b.gpu];
		const noGpu = host && host.gpus.length === 0;
		out.push({
			kind: 'gpu',
			text,
			tone: noGpu && b.gpu === 'required' ? 'bad' : 'accent',
			title: host ? (host.gpus.length ? `This server: ${host.gpus.join('; ')}` : 'No graphics chip found on this server') : undefined
		});
	}
	if (b.https !== 'no') {
		out.push({
			kind: 'https',
			text: b.https === 'required' ? 'Needs HTTPS' : 'HTTPS recommended',
			tone: 'info',
			title:
				b.https === 'required'
					? 'Hearth turns HTTPS on for this app when installing it'
					: 'Some features (phone apps, camera) work best over HTTPS'
		});
	}
	if (b.host_network) out.push({ kind: 'net', text: 'Host network', title: 'Sees your home network directly (for finding devices)' });
	return out;
}
