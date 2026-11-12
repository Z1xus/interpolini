<script lang="ts">
	import Marks from "#lib/components/Marks.svelte";
	import { gridOf, moment, stills, recordings, grids } from "#lib/assets.ts";
	import Preview from "#lib/components/Preview.svelte";
	import type { Focus } from "#lib/focus.ts";
	import type { Playback } from "#lib/playback.svelte.ts";
	import { usePane } from "#lib/pane.svelte.ts";
	import { settingsOf, planOf, keyFrameOf, sourceTimeOf } from "#lib/show.ts";
	import type { Store } from "#lib/store.svelte.ts";
	import { isDark, watchTheme } from "#lib/theme.ts";
	import { loadUi, type Ui } from "#lib/ui.ts";
	import { aspect, panels, regions, stages, type Rect } from "#lib/window.ts";
	import type { Attachment } from "svelte/attachments";
	import { MediaQuery } from "svelte/reactivity";

	let {
		focus,
		mark,
		playback,
		store,
		status = $bindable(),
		broken = $bindable()
	}: {
		focus: Focus;
		mark?: Rect;
		playback: Playback;
		store: Store;
		status: "loading" | "ready" | "failed";
		broken: boolean;
	} = $props();

	const id = $props.id();
	const pane = usePane();
	const reduced = new MediaQuery("prefers-reduced-motion: reduce");
	let ui = $state<Ui>();
	let thumbs = $state<Record<string, Uint8Array>>({});
	let width = $state(0);
	let canvasWidth = $state(0);
	let dialogWidth = $state(0);
	let dialogNeeded = $state(false);

	const view = $derived(focus.view ?? "normal");
	const clip = $derived(
		focus.show
			? "blend" in focus.show
				? moment.clip
				: (gridOf(focus.show.grid).clip ?? "240.mp4")
			: stills.clip
	);
	const panel = $derived(focus.panel && panels[focus.panel]);
	const settings = $derived([
		...(panel?.settings ?? []),
		...(focus.show ? settingsOf(focus.show) : [])
	]);
	const plan = $derived(planOf(settings, focus.show));
	const weights = $derived(Float32Array.from(plan.weights));
	const { x, y, w, h } = regions.preview;
	const scene = $derived(focus.scene ?? "plain");
	const placed = $derived(stages[scene]);
	const dialog = $derived("dialog" in placed ? placed.dialog : undefined);
	const previewHeight = $derived((width / aspect) * h);
	const marked = $derived(
		!playback.playing && playback.frame === (focus.frame ?? keyFrameOf(focus.show))
	);

	const nextFrames = async (count: number) => {
		for (let frame = 0; frame < count; frame++) {
			await new Promise(requestAnimationFrame);
		}
	};

	const mount: Attachment<HTMLCanvasElement> = (canvas) => {
		let loaded: Ui | undefined;
		let removed = false;
		const refit = () => loaded?.fit();
		loadUi()
			.then(async ({ ui: module, pictures, wave, frames, thumbs: loadedThumbs }) => {
				if (removed) return;
				module.mount(canvas.id, pictures, wave, frames);
				loaded = module;
				module.theme(isDark());
				await nextFrames(3);
				if (removed) return;
				thumbs = loadedThumbs;
				ui = module;
				await nextFrames(3);
				if (removed) return;
				status = "ready";
			})
			.catch((error) => {
				console.error(error);
				status = "failed";
			});
		const stopWatching = watchTheme((dark) => loaded?.theme(dark));
		addEventListener("resize", refit);
		return () => {
			removed = true;
			stopWatching();
			removeEventListener("resize", refit);
			loaded?.unmount();
			ui = undefined;
		};
	};

	const mountDialog: Attachment<HTMLCanvasElement> = (canvas) => {
		ui?.mount_dialog(canvas.id);
		ui?.theme(isDark());
	};

	$effect(() => {
		ui?.motion(reduced.current);
	});

	$effect(() => {
		ui?.open(panel?.open ?? "interpolation");
	});

	$effect(() => {
		ui?.scene(scene);
		if (thumbs[clip]) ui?.thumbnails(thumbs[clip]);
	});

	$effect(() => {
		if (dialog) dialogNeeded = true;
	});

	$effect(() => {
		ui?.configure(settings, weights, plan.source, plan.output, clip, recordings[clip]);
	});

	$effect(() => {
		if (!playback.playing)
			ui?.position(
				focus.show ? sourceTimeOf(focus.show, playback.frame) : grids.compare.span[0] + stills.at
			);
	});
</script>

<div
	bind:clientWidth={width}
	class={["absolute inset-0 transition-opacity duration-300", status !== "ready" && "opacity-0"]}
>
	<canvas
		{id}
		{@attach mount}
		bind:clientWidth={canvasWidth}
		style:scale={canvasWidth ? width / canvasWidth : 0}
		class="pointer-events-none absolute start-0 top-0 origin-top-left"
	></canvas>
	<div
		class="absolute"
		style:left="{x * 100}%"
		style:top="{y * 100}%"
		style:width="{w * 100}%"
		style:height="{h * 100}%"
	>
		{#if focus.show}
			<Preview show={focus.show} {view} {playback} {store} bind:broken />
		{/if}
		{#key pane.active}
			<div class={["pointer-events-none absolute inset-0", !marked && "invisible"]}>
				{#if focus.marks}
					<Marks marks={focus.marks} width={width * w} height={previewHeight} />
				{/if}
			</div>
		{/key}
	</div>
	<div
		class={[
			"pointer-events-none absolute inset-0 bg-black/45 transition-opacity duration-300",
			!dialog && "opacity-0"
		]}
	></div>
	{#if ui && dialogNeeded}
		<div
			class={[
				"pointer-events-none absolute overflow-hidden rounded-md border border-seam shadow-lg transition-opacity duration-300",
				!dialog && "opacity-0"
			]}
			style:left="{(dialog?.x ?? 0) * 100}%"
			style:top="{(dialog?.y ?? 0) * 100}%"
			style:width="{(dialog?.w ?? 0) * 100}%"
			style:height="{(dialog?.h ?? 0) * 100}%"
		>
			<canvas
				id="{id}-dialog"
				{@attach mountDialog}
				bind:clientWidth={dialogWidth}
				style:scale={dialogWidth && dialog ? (width * dialog.w) / dialogWidth : 0}
				class="absolute start-0 top-0 origin-top-left"
			></canvas>
		</div>
	{/if}
	{#if mark}
		<div
			class="pointer-events-none absolute rounded-[calc(3px/var(--zoom))] outline-[length:calc(2px/var(--zoom))] outline-offset-[calc(1px/var(--zoom))] outline-accent"
			style:left="{mark.x * 100}%"
			style:top="{mark.y * 100}%"
			style:width="{mark.w * 100}%"
			style:height="{mark.h * 100}%"
		></div>
	{/if}
</div>
