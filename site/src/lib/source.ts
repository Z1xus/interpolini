import { gridRate, moment, shapeOf, variantNames } from "#lib/assets.ts";
import { sizeOf, type Resource } from "#lib/loader.ts";
import type { Renderer } from "#lib/renderer.ts";
import { settingsOf, planOf, type Show, type View } from "#lib/show.ts";
import { fixed } from "#lib/weights.ts";

type Source = {
	frames: number;
	seek(frame: number): (() => void) | undefined;
	play?(from: number, speed: number, onframe: (frame: number) => void): () => void;
};

function videoSource(video: HTMLVideoElement, draw: () => void, fail: () => void): Source {
	const frames = Math.round(video.duration * gridRate);
	const timeOf = (frame: number) => (frame + 0.5) / gridRate;
	const near = (time: number) => Math.abs(video.currentTime - time) < 0.25 / gridRate;
	return {
		frames,
		seek(frame) {
			const time = timeOf(frame);
			if (near(time) && !video.seeking) {
				draw();
				return undefined;
			}
			video.onseeked = draw;
			if (!near(time)) video.currentTime = time;
			return () => {
				video.onseeked = null;
			};
		},
		play(from, speed, onframe) {
			const time = timeOf(from);
			if (!near(time)) video.currentTime = time;
			video.playbackRate = speed;
			video.play().catch((error: DOMException) => {
				if (error.name === "AbortError") return;
				console.error(error);
				fail();
			});
			let request = 0;
			const tick: VideoFrameRequestCallback = (_now, { mediaTime }) => {
				draw();
				onframe(Math.min(Math.round(mediaTime * gridRate), frames - 1));
				request = video.requestVideoFrameCallback(tick);
			};
			request = video.requestVideoFrameCallback(tick);
			return () => {
				video.cancelVideoFrameCallback(request);
				video.pause();
			};
		}
	};
}

function stackSource(draw: (frame: number) => void): Source {
	const frames = moment.frames;
	return {
		frames,
		seek(frame) {
			draw(frame);
			return undefined;
		},
		play(from, speed, onframe) {
			let request = 0;
			let start = 0;
			let shown = -1;
			const tick = (now: number) => {
				start ||= now;
				const frame =
					(from + Math.floor(((now - start) / 1000) * moment.playback * speed)) % frames;
				if (frame !== shown) {
					shown = frame;
					draw(frame);
					onframe(frame);
				}
				request = requestAnimationFrame(tick);
			};
			request = requestAnimationFrame(tick);
			return () => cancelAnimationFrame(request);
		}
	};
}

export function sourceOf(
	renderer: Renderer,
	show: Show,
	resource: Resource,
	view: View,
	fail: () => void
) {
	if ("grid" in show) {
		const sheet = { ...sizeOf(resource), ...shapeOf(show.grid) };
		const index = variantNames(show.grid).indexOf(show.variant);
		const draw = () => renderer.tiles(resource, sheet, index, view);
		return resource instanceof HTMLVideoElement
			? videoSource(resource, draw, fail)
			: { frames: 0, seek: () => void draw() };
	}
	if (resource instanceof ImageBitmap && "blend" in show) {
		const plan = fixed(planOf(settingsOf(show), show).weights);
		return stackSource((frame) => renderer.frames(resource, plan, show.blend, frame, view));
	}
}

export const pixelsOf = (show: Show, resource: Resource) =>
	sizeOf(resource).width / ("grid" in show ? shapeOf(show.grid).columns : moment.columns);
