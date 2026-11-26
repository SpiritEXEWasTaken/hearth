import { followJob, errorText, type Job } from '#lib/api.ts';

/** State for one change shown in a JobPanel. */
export class JobRun {
	title = $state('');
	lines = $state<string[]>([]);
	job = $state<Job | null>(null);
	error = $state('');
	active = $state(false);
	open = $state(false);

	/** Starts a change: `start` returns {job: id}; resolves when the job ends. */
	async run(title: string, start: () => Promise<{ job: string }>): Promise<Job | null> {
		this.title = title;
		this.lines = [];
		this.job = null;
		this.error = '';
		this.active = true;
		this.open = true;
		try {
			const { job } = await start();
			const done = await followJob(job, (l) => this.lines.push(l));
			this.job = done;
			return done;
		} catch (e) {
			this.error = errorText(e);
			return null;
		} finally {
			this.active = false;
		}
	}
}
