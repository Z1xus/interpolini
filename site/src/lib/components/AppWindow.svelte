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
	import { loadThumbs, loadUi, type Ui } from "#lib/ui.ts";
	import {
		aspect,
		dialogSize,
		panels,
		regions,
		stages,
		windowSize,
		type Rect
	} from "#lib/window.ts";
	import { untrack } from "svelte";
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
	let width = $state(0);
	let ratio = $state(1);
	let previewNeeded = $state(false);
	let dialogNeeded = $state(false);
	let dialogReady = $state(false);
	let dialogOpen = $state(false);
	let lastDialog = $state<Rect>();
	let appliedScene = $state("");
	let appliedClip = stills.clip;
	let appliedOpen = "";
	let appliedConfig = "";
	let appliedPosition = Number.NaN;
	let lastScroll = -Infinity;

	const density = $derived(Math.min(2 * ratio, 4));
	const canvasBox = $derived({
		w: Math.floor(windowSize.width * density) / ratio,
		h: Math.floor(windowSize.height * density) / ratio
	});
	const dialogCanvasBox = $derived({
		w: Math.floor(dialogSize.width * density) / ratio,
		h: Math.floor(dialogSize.height * density) / ratio
	});

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
	const dialogBox = $derived(dialog ?? lastDialog);
	const previewHeight = $derived((width / aspect) * h);
	const marked = $derived(
		!playback.playing && playback.frame === (focus.frame ?? keyFrameOf(focus.show))
	);

	const nextFrames = async (count: number) => {
		for (let frame = 0; frame < count; frame++) {
			await new Promise(requestAnimationFrame);
		}
	};

	const applyConfig = (current: Ui, force = false) => {
		const key = `${settings.join("\0")}|${weights.join(",")}|${plan.source}|${plan.output}|${clip}`;
		if (!force && key === appliedConfig) return;
		appliedConfig = key;
		current.configure(settings, weights, plan.source, plan.output, clip, recordings[clip]);
	};

	const applyPosition = (current: Ui, force = false) => {
		if (playback.playing) return;
		const seconds = focus.show
			? sourceTimeOf(focus.show, playback.frame)
			: grids.compare.span[0] + stills.at;
		if (!force && seconds === appliedPosition) return;
		appliedPosition = seconds;
		current.position(seconds);
	};

	const mount: Attachment<HTMLCanvasElement> = (canvas) => {
		let loaded: Ui | undefined;
		let removed = false;
		let resumeTimer: ReturnType<typeof setTimeout> | undefined;
		ratio = devicePixelRatio;
		const refit = () => {
			if (devicePixelRatio === ratio) return;
			ratio = devicePixelRatio;
			loaded?.fit();
		};
		const onScroll = () => {
			lastScroll = performance.now();
			if (status !== "ready") return;
			loaded?.pause(true);
			clearTimeout(resumeTimer);
			resumeTimer = setTimeout(() => loaded?.pause(false), 120);
		};
		loadUi()
			.then(async ({ ui: module, pictures, wave, frames }) => {
				if (removed) return;
				appliedClip = stills.clip;
				appliedOpen = "";
				appliedConfig = "";
				appliedPosition = Number.NaN;
				appliedScene = "";
				dialogOpen = false;
				module.mount(canvas.id, pictures, wave, frames, isDark(), reduced.current);
				loaded = module;
				ui = module;
				await nextFrames(3);
				if (removed) return;
				if (dialog && !dialogReady) await nextFrames(2);
				if (removed) return;
				status = "ready";
				if (performance.now() - lastScroll < 120) {
					module.pause(true);
					clearTimeout(resumeTimer);
					resumeTimer = setTimeout(() => module.pause(false), 120);
				}
			})
			.catch((error) => {
				console.error(error);
				status = "failed";
			});
		const stopWatching = watchTheme((dark) => loaded?.theme(dark));
		addEventListener("resize", refit);
		addEventListener("scroll", onScroll, { passive: true });
		return () => {
			removed = true;
			clearTimeout(resumeTimer);
			stopWatching();
			removeEventListener("resize", refit);
			removeEventListener("scroll", onScroll);
			loaded?.unmount();
			ui = undefined;
		};
	};

	const mountDialog: Attachment<HTMLCanvasElement> = (canvas) => {
		let cancelled = false;
		ui?.mount_dialog(canvas.id, isDark());
		nextFrames(2).then(() => {
			if (!cancelled) dialogReady = true;
		});
		return () => {
			cancelled = true;
			dialogReady = false;
		};
	};

	$effect(() => {
		if (focus.show) previewNeeded = true;
	});

	$effect(() => {
		if (status !== "ready" || (previewNeeded && dialogNeeded)) return;
		const schedule = (delay: number, run: () => void) => {
			let timer: ReturnType<typeof setTimeout>;
			const check = () => {
				const idle = performance.now() - lastScroll;
				if (idle < 400) timer = setTimeout(check, 400 - idle);
				else run();
			};
			timer = setTimeout(check, delay);
			return () => clearTimeout(timer);
		};
		const stopPreview = previewNeeded ? undefined : schedule(900, () => (previewNeeded = true));
		const stopDialog = dialogNeeded ? undefined : schedule(950, () => (dialogNeeded = true));
		return () => {
			stopPreview?.();
			stopDialog?.();
		};
	});

	$effect(() => {
		ui?.motion(reduced.current);
	});

	$effect(() => {
		const target = panel?.open ?? "interpolation";
		if (!ui || target === appliedOpen) return;
		appliedOpen = target;
		ui.open(target);
	});

	$effect(() => {
		const current = ui;
		const target = scene;
		if (!current || target === untrack(() => appliedScene)) return;
		const apply = () => {
			const previous = untrack(() => appliedScene);
			appliedScene = target;
			current.scene(target);
			untrack(() => {
				if (clip !== stills.clip || previous.startsWith("mask-") || target.startsWith("mask-")) {
					applyConfig(current, true);
				}
				if (focus.show) applyPosition(current, true);
			});
		};
		if (untrack(() => status !== "ready") || performance.now() - lastScroll > 120) {
			apply();
			return;
		}
		let timer = setTimeout(apply, 110);
		const defer = () => {
			clearTimeout(timer);
			timer = setTimeout(apply, 110);
		};
		addEventListener("scroll", defer, { passive: true });
		return () => {
			clearTimeout(timer);
			removeEventListener("scroll", defer);
		};
	});

	$effect(() => {
		const current = ui;
		const target = clip;
		if (!current || target === appliedClip) return;
		let cancelled = false;
		const file = target === stills.clip ? "thumbs.webp" : "thumbs-sword.webp";
		loadThumbs(file)
			.then((bytes) => {
				if (cancelled || clip !== target) return;
				appliedClip = target;
				current.thumbnails(bytes);
			})
			.catch(console.error);
		return () => {
			cancelled = true;
		};
	});

	$effect(() => {
		if (dialog) {
			lastDialog = dialog;
			if (appliedScene === scene) {
				dialogNeeded = true;
				dialogOpen = true;
			}
		} else {
			dialogOpen = false;
		}
	});

	$effect(() => {
		if (ui) applyConfig(ui);
	});

	$effect(() => {
		if (ui) applyPosition(ui);
	});
</script>

<div bind:clientWidth={width} class="absolute inset-0">
	<canvas
		{id}
		{@attach mount}
		style:--w="{canvasBox.w}px"
		style:--h="{canvasBox.h}px"
		style:scale={width / canvasBox.w}
		class="pointer-events-none absolute start-0 top-0 origin-top-left h-(--h)! w-(--w)! max-h-none! max-w-none! min-h-0! min-w-0!"
	></canvas>
	<div
		class="absolute"
		style:left="{x * 100}%"
		style:top="{y * 100}%"
		style:width="{w * 100}%"
		style:height="{h * 100}%"
	>
		{#if previewNeeded}
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
			"pointer-events-none absolute inset-0 bg-black/45 [transition:opacity_300ms,visibility_300ms]",
			(!dialogOpen || !dialogReady) && "invisible opacity-0"
		]}
	></div>
	{#if ui && dialogNeeded}
		<div
			class={[
				"pointer-events-none absolute overflow-hidden rounded-md border border-seam shadow-lg [transition:opacity_300ms,visibility_300ms]",
				(!dialogOpen || !dialogReady) && "invisible opacity-0"
			]}
			style:left="{(dialogBox?.x ?? 0) * 100}%"
			style:top="{(dialogBox?.y ?? 0) * 100}%"
			style:width="{(dialogBox?.w ?? 0) * 100}%"
			style:height="{(dialogBox?.h ?? 0) * 100}%"
		>
			<canvas
				id="{id}-dialog"
				{@attach mountDialog}
				style:--w="{dialogCanvasBox.w}px"
				style:--h="{dialogCanvasBox.h}px"
				style:scale={dialogBox ? (width * dialogBox.w) / dialogCanvasBox.w : 0}
				class="absolute start-0 top-0 origin-top-left h-(--h)! w-(--w)! max-h-none! max-w-none! min-h-0! min-w-0!"
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
