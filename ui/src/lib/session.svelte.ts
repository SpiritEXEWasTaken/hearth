import type { Me } from './types';
import { setCsrf } from './api';

class Session {
	me = $state<Me | null>(null);
	hostname = $state('');

	set(me: Me | null) {
		this.me = me;
		setCsrf(me?.csrf ?? '');
	}

	get isAdmin() {
		return this.me?.user.role === 'admin';
	}
}

export const session = new Session();

type Theme = 'system' | 'light' | 'dark';

class ThemeState {
	value = $state<Theme>('system');

	init() {
		const saved = localStorage.getItem('hearth-theme') as Theme | null;
		this.apply(saved ?? 'system');
	}

	apply(t: Theme) {
		this.value = t;
		localStorage.setItem('hearth-theme', t);
		if (t === 'system') document.documentElement.removeAttribute('data-theme');
		else document.documentElement.setAttribute('data-theme', t);
	}

	cycle() {
		this.apply(this.value === 'system' ? 'light' : this.value === 'light' ? 'dark' : 'system');
	}
}

export const theme = new ThemeState();
