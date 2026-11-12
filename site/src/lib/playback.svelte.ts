export const speeds = [1, 0.25, 0.125] as const;

export class Playback {
	frame = $state(0);
	frames = $state(0);
	playing = $state(false);
	speed = $state<number>(speeds[0]);

	toggle = () => {
		if (this.frames > 0) this.playing = !this.playing;
	};

	step(delta: number) {
		this.playing = false;
		this.frame = Math.min(Math.max(this.frame + delta, 0), Math.max(this.frames - 1, 0));
	}

	stop() {
		this.playing = false;
	}
}
