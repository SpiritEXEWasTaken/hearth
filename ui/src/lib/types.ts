export interface User {
	id: string;
	username: string;
	role: 'admin' | 'viewer';
	created_at: number;
	last_login_at: number | null;
}

export interface Me {
	user: User;
	csrf: string;
	scopes: string[];
}

export interface Disk {
	mount: string;
	device: string;
	fs_type: string;
	total_bytes: number;
	used_bytes: number;
	available_bytes: number;
}

export interface HostStats {
	hostname: string;
	os: string;
	kernel: string;
	arch: string;
	uptime_secs: number;
	load: [number, number, number];
	cpu_count: number;
	cpu_percent: number;
	memory: {
		total_bytes: number;
		available_bytes: number;
		used_bytes: number;
		swap_total_bytes: number;
		swap_used_bytes: number;
	};
	memory_pressure_avg60: number | null;
	oom_kills: number | null;
	disks: Disk[];
}

export interface Container {
	id: string;
	name: string;
	service: string | null;
	state: string;
	status: string;
	health: string | null;
	image: string;
	memory_bytes: number | null;
	restart_count: number | null;
	started_at: string | null;
}

export interface Latest {
	checked_at: number;
	digest: string | null;
	version: string | null;
	update_available: boolean;
	newer_in_track: string | null;
	newer_major: string | null;
	error: string | null;
}

export interface ImageVersion {
	service: string;
	reference: string;
	running_digest: string | null;
	running_version: string | null;
	latest: Latest | null;
}

export interface Unit {
	name: string;
	user?: string;
	load_state: string;
	active_state: string;
	sub_state: string;
	unit_file_state: string;
	[k: string]: unknown;
}

export type AppKind = 'compose' | 'service' | 'native' | 'link' | 'package';
export type AppState = 'running' | 'partial' | 'stopped' | 'unhealthy';

export interface App {
	id: string;
	title: string;
	kind: AppKind;
	dashboard: boolean;
	managed_by: 'hearth' | 'casaos' | 'compose' | 'container';
	endpoint?: { kind: string; [k: string]: unknown };
	unit?: Unit;
	read_only: boolean;
	state: AppState;
	health: string | null;
	memory_bytes: number | null;
	web_url: string | null;
	icon: string | null;
	containers: Container[];
	images: ImageVersion[];
}

export interface AppDetail extends App {
	working_dir: string | null;
	config_files: string[];
	external_variables: string[];
	missing_devices: string[];
	store_app_id: string | null;
}

export interface Status {
	host: HostStats;
	apps: App[];
	warnings: string[];
}

export interface ApplyPlan {
	app: string;
	compose_diff: string;
	env_changes: string[];
	decisions: string[];
	errors: string[];
	warnings: string[];
	unchanged: boolean;
}

export interface UpdatePlan {
	app: string;
	services: {
		service: string;
		reference: string;
		running_version: string | null;
		new_version: string | null;
		running_digest: string | null;
		new_digest: string | null;
		newer_in_track: string | null;
		newer_major: string | null;
	}[];
	backup_first: boolean;
}

export interface HistoryEntry {
	revision: number;
	time: number;
	actor: string;
	message: string;
}

export interface BackupInfo {
	file: string;
	size_bytes: number;
	created_at: number;
}

export interface TlsInfo {
	tailscale_names: string[];
	local_names: string[];
	ca_fingerprint_sha256: string | null;
}

export interface TokenInfo {
	id: string;
	name: string;
	scopes: string[];
	created_at: number;
	last_used_at: number | null;
}
