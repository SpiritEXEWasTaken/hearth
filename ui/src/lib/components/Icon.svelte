<script lang="ts">
	// A small set of line icons (24px grid, stroke-based).
	const paths: Record<string, string> = {
		home: 'M3 10.5 12 3l9 7.5V20a1 1 0 0 1-1 1h-5v-6h-6v6H4a1 1 0 0 1-1-1z',
		apps: 'M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z',
		settings:
			'M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z',
		external: 'M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5',
		play: 'M7 4.5v15l12-7.5z',
		stop: 'M6 6h12v12H6z',
		restart: 'M4 4v6h6M20 20v-6h-6M5.5 15a7.5 7.5 0 0 0 13.4 1.5M18.5 9A7.5 7.5 0 0 0 5.1 7.5',
		update: 'M12 3v12M7 10l5 5 5-5M5 21h14',
		logs: 'M5 4h14v16H5zM8 8h8M8 12h8M8 16h5',
		history: 'M3 12a9 9 0 1 0 3-6.7L3 8M3 3v5h5M12 7v5l3 2',
		backup: 'M4 7h16v13H4zM4 7l2-3h12l2 3M9 12h6',
		edit: 'M4 20h4L19 9l-4-4L4 16zM14 6l4 4',
		info: 'M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20zM12 16v-4M12 8h.01',
		sun: 'M12 17a5 5 0 1 0 0-10 5 5 0 0 0 0 10zM12 1v2M12 21v2M4.2 4.2l1.4 1.4M18.4 18.4l1.4 1.4M1 12h2M21 12h2M4.2 19.8l1.4-1.4M18.4 5.6l1.4-1.4',
		moon: 'M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z',
		auto: 'M12 22a10 10 0 1 0 0-20zM12 2a10 10 0 0 1 0 20',
		logout: 'M9 21H5a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h4M16 17l5-5-5-5M21 12H9',
		user: 'M12 12a4.5 4.5 0 1 0 0-9 4.5 4.5 0 0 0 0 9zM4 21a8 8 0 0 1 16 0',
		lock: 'M6 11h12v10H6zM8 11V7a4 4 0 0 1 8 0v4',
		close: 'M6 6l12 12M18 6 6 18',
		check: 'M5 12.5l4.5 4.5L19 7',
		alert: 'M12 3 2 20h20zM12 10v4M12 17h.01',
		search: 'M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14zM21 21l-5-5',
		cpu: 'M7 7h10v10H7zM9 1v3M15 1v3M9 20v3M15 20v3M1 9h3M1 15h3M20 9h3M20 15h3',
		memory: 'M3 7h18v10H3zM7 7v10M11 7v10M15 7v10M3 20h18',
		disk: 'M3 12h18M5 5h14l2 7v6a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-6zM7 16h.01',
		more: 'M5 12h.01M12 12h.01M19 12h.01',
		chevron: 'M9 6l6 6-6 6',
		back: 'M15 6l-6 6 6 6',
		download: 'M12 4v11M7 10l5 5 5-5M4 20h16',
		key: 'M15 7a4 4 0 1 1-3.5 6L4 20.5V17h3v-3h3l1.5-1.5A4 4 0 0 1 15 7zM16 8h.01',
		shield: 'M12 2 4 5v6c0 5 3.4 9.3 8 11 4.6-1.7 8-6 8-11V5z',
		plus: 'M12 5v14M5 12h14',
		trash: 'M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3',
		box: 'M3 7.5 12 3l9 4.5v9L12 21l-9-4.5zM3 7.5 12 12l9-4.5M12 12v9',
		store: 'M5 8h14l-1.2 12.1a1 1 0 0 1-1 .9H7.2a1 1 0 0 1-1-.9zM9 8V6a3 3 0 0 1 6 0v2'
	};
	let { name, size = 20, stroke = 1.8 }: { name: string; size?: number; stroke?: number } = $props();
</script>

<svg
	width={size}
	height={size}
	viewBox="0 0 24 24"
	fill={name === 'play' || name === 'stop' ? 'currentColor' : 'none'}
	stroke="currentColor"
	stroke-width={stroke}
	stroke-linecap="round"
	stroke-linejoin="round"
	aria-hidden="true"
>
	<path d={paths[name] ?? paths.info} />
</svg>
