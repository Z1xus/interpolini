<script lang="ts">
	import { createRenderer, type Renderer } from "#lib/renderer.ts";
	import { assetOf, type Show, type View } from "#lib/show.ts";
	import type { Playback } from "#lib/playback.svelte.ts";
	import { sourceOf } from "#lib/source.ts";
	import type { Store } from "#lib/store.svelte.ts";
	import { untrack } from "svelte";
	import type { Attachment } from "svelte/attachments";

	let {
		show,
		view,
		playback,
		store,
		broken = $bindable()
	}: { show: Show; view: View; playback: Playback; store: Store; broken: boolean } = $props();

	let renderer = $state<Renderer>();
	let visible = $state(true);

	const setup: Attachment<HTMLCanvasElement> = (canvas) => {
		const build = () => {
			try {
				renderer = createRenderer(canvas);
				broken = false;
			} catch (error) {
				console.error(error);
				broken = true;
			}
		};
		const lose = (event: Event) => {
			event.preventDefault();
			renderer = undefined;
			broken = true;
		};
		build();
		canvas.addEventListener("webglcontextlost", lose);
		canvas.addEventListener("webglcontextrestored", build);
		const observer = new IntersectionObserver(([entry]) => (visible = entry.isIntersecting));
		observer.observe(canvas);
		return () => {
			observer.disconnect();
			canvas.removeEventListener("webglcontextlost", lose);
			canvas.removeEventListener("webglcontextrestored", build);
			renderer?.dispose();
			renderer = undefined;
		};
	};

	const source = $derived.by(() => {
		const entry = store.get(assetOf(show));
		if (renderer && entry?.status === "ready")
			return sourceOf(renderer, show, entry.resource, view, () => store.fail(assetOf(show)));
	});

	$effect(() => {
		if (store.get(assetOf(show))?.status === "failed") playback.stop();
	});

	$effect(() => () => {
		playback.frames = 0;
		playback.stop();
	});

	$effect(() => {
		playback.frames = source?.frames ?? 0;
		untrack(() => {
			if (playback.frames > 0) playback.frame = Math.min(playback.frame, playback.frames - 1);
		});
	});

	$effect(() => {
		if (source && !playback.playing) return source.seek(playback.frame);
	});

	$effect(() => {
		if (!source?.play || !playback.playing || !visible) return;
		return source.play(
			untrack(() => playback.frame),
			playback.speed,
			(frame) => (playback.frame = frame)
		);
	});
</script>

<canvas {@attach setup} class={["absolute inset-0 size-full bg-black", broken && "invisible"]}
></canvas>
