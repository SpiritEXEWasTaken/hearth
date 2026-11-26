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
	https_port?: number;
	https_subdomain?: string;
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

// ----- system updates and packages -----

export interface BackendInfo {
	kind: string;
	available: boolean;
	detail: string;
	note?: string;
}

export interface RebootStatus {
	needed: boolean;
	reasons: string[];
}

export interface SystemUpdate {
	id: string;
	backend: string;
	name: string;
	arch?: string;
	current_version?: string;
	new_version: string;
	repo?: string;
	security: boolean;
	severity: string;
	summary: string;
}

export interface SystemState {
	info: { distro_id: string; distro_name: string; kernel: string; backends: BackendInfo[]; reboot: RebootStatus };
	updates: { checked_at: number; updates: SystemUpdate[]; errors: string[]; reboot: RebootStatus };
}

export interface UpgradePlan {
	updates: SystemUpdate[];
	notes: string[];
	checked_at: number;
}

export interface PackageEntry {
	backend: string;
	name: string;
	version: string;
	arch?: string;
	repo?: string;
	installed: boolean;
	summary: string;
}

export interface PackageDetail extends PackageEntry {
	installed_version?: string;
	description: string;
	url?: string;
	license?: string;
	size_bytes?: number;
}

export interface PackagePlan {
	action: 'install' | 'remove';
	backend: string;
	names: string[];
	install: PackageEntry[];
	remove: PackageEntry[];
	update: PackageEntry[];
	warnings: string[];
	errors: string[];
}

export interface Setting {
	value: string;
	description: string;
}

export interface HttpsView {
	enabled: boolean;
	port: number | null;
	target_port: number | null;
	urls: string[];
	running: boolean;
	unavailable: string | null;
	suggested_port: number | null;
	home_assistant: boolean;
	subdomain: string | null;
	suggested_subdomain: string | null;
	domain: string | null;
}

export interface HttpsTest {
	ok: boolean;
	status: number | null;
	message: string;
	fix: string | null;
}

export interface DomainStatus {
	configured: boolean;
	name: string | null;
	base: string | null;
	address: string | null;
	ip: string | null;
	certificates: { name: string; expires_at: number }[];
	last_ok: number | null;
	last_error: string | null;
}

// ----- store (M6a) -----

export type StoreBadges = {
	ram_mb: number;
	ram_recommended_mb?: number;
	data_mb: number;
	download_mb?: Record<string, number>;
	architectures: string[];
	gpu: 'none' | 'optional' | 'recommended' | 'required';
	https: 'no' | 'recommended' | 'required';
	host_network: boolean;
	notice: boolean;
	screenshots: number;
};

export type StoreApp = {
	id: string;
	name: string;
	summary: string;
	category: string;
	tags?: string[];
	app_name: string;
	badges: StoreBadges;
	installed: string[];
};

export type HostFacts = {
	arch: string;
	mem_total_mb: number;
	mem_available_mb: number;
	disk_free_mb: number | null;
	gpus: string[];
};

export type StoreList = {
	source: string | null;
	generated: number | null;
	refreshed_at: number | null;
	error: string | null;
	categories: { id: string; name: string }[];
	apps: StoreApp[];
	host: HostFacts;
};

export type StoreChoice = { id: string; label: string; help?: string; needs?: string };

export type StoreOption = {
	id: string;
	kind: 'choice' | 'gpu' | 'switch' | 'folder' | 'text';
	label: string;
	help?: string;
	default: string;
	choices?: StoreChoice[];
	env?: string;
};

export type StoreEntry = {
	id: string;
	name: string;
	summary: string;
	description: string;
	category: string;
	upstream: { website: string; source: string; docs?: string };
	license: string;
	screenshots?: { file: string; caption: string; source: string; license?: string }[];
	app_name: string;
	web?: { https: 'no' | 'recommended' | 'required'; dashboard: boolean };
	notice?: string;
	ports?: { env?: string; port: number; protocol: string; label: string; movable: boolean }[];
	options?: StoreOption[];
	after_install?: string[];
	requirements: { ram_mb: number; data_mb: number; architectures: string[]; host_network: boolean };
};

export type StoreCheck = { level: 'ok' | 'info' | 'warn' | 'error'; topic: string; message: string };

export type InstallPlan = {
	entry: string;
	name: string;
	answers: Record<string, string>;
	options: { id: string; value: string; unavailable: Record<string, string> }[];
	variants: string[];
	ports: { env: string | null; label: string; port: number; protocol: string; default: number; movable: boolean }[];
	checks: StoreCheck[];
	ok: boolean;
	download_mb: number;
	disk_needed_mb: number;
	https: boolean;
	data_dir: string;
	compose: string;
};

export type StoreEntryView = {
	app: StoreApp & { badges: StoreBadges };
	entry: StoreEntry;
	installed: string[];
	plan: InstallPlan;
};

export type AppStoreInfo = {
	entry: string | null;
	how: 'installed' | 'linked' | null;
	name: string | null;
	notice: string | null;
	after_install: string[];
	suggestion: { id: string; name: string } | null;
	credentials: string[];
};
