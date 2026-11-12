<script lang="ts">
	import AppWindow from "#lib/components/AppWindow.svelte";
	import Chip from "#lib/components/Chip.svelte";
	import PlaybackBar from "#lib/components/PlaybackBar.svelte";
	import { camera, reach } from "#lib/focus.ts";
	import { usePane } from "#lib/pane.svelte.ts";
	import { assetOf } from "#lib/show.ts";
	import { pixelsOf } from "#lib/source.ts";
	import { stills } from "#lib/assets.ts";
	import { aspect, regions } from "#lib/window.ts";
	import { MediaQuery } from "svelte/reactivity";

	let { class: className }: { class?: string } = $props();
	const pane = usePane();
	const band = new MediaQuery("width < 64rem");
	let width = $state(0);
	let height = $state(0);
	let status = $state<"loading" | "ready" | "failed">("loading");
	let broken = $state(false);
	let attempt = $state(0);

	const focus = $derived(pane.focus ?? { rect: regions.window });
	const name = $derived(focus.show && assetOf(focus.show));
	const entry = $derived(name && pane.store.get(name));
	const pixels = $derived(
		focus.show && entry?.status === "ready" ? pixelsOf(focus.show, entry.resource) : stills.width
	);
	const shape = $derived(camera(focus, band.current, width, height, pixels));
	const problem = $derived(status === "failed" || broken || entry?.status === "failed");
	const waiting = $derived(status === "loading" || entry?.status === "loading");

	const retry = () => {
		if (status === "failed" || broken) {
			status = "loading";
			broken = false;
			attempt++;
		}
		if (name && entry?.status === "failed") pane.store.retry(name);
	};

	const keys: Record<string, (event: KeyboardEvent) => void> = {
		ArrowLeft: () => pane.playback.step(-1),
		ArrowRight: () => pane.playback.step(1),
		" ": (event) => event.repeat || pane.playback.toggle()
	};

	const press = (event: KeyboardEvent) => {
		const act = keys[event.key];
		const control = event.target instanceof Element && event.target.closest("button, a, input");
		if (!act || control instanceof HTMLInputElement || (event.key === " " && control)) return;
		event.preventDefault();
		if (event.type === "keydown") act(event);
	};
</script>

<div role="presentation" onkeydown={press} onkeyup={press} class={["bg-rack", className]}>
	<div
		role="group"
		aria-label="The interpolini window"
		bind:clientWidth={width}
		bind:clientHeight={height}
		style:--aspect={aspect}
		class="relative grid h-(--band) place-items-center overflow-hidden border-y border-seam bg-void [container-type:size] lg:aspect-(--aspect) lg:h-auto lg:rounded-md lg:border-x"
	>
		<div
			inert
			class="relative transition-[scale,translate] duration-[900ms] ease-[cubic-bezier(0.22,1,0.36,1)]"
			style:aspect-ratio={aspect}
			style:width="min(100cqw, {aspect * 100}cqh, {reach(pixels)}px)"
			style:scale={shape.scale}
			style:translate="{shape.x}px {shape.y}px"
			style:--zoom={shape.scale}
		>
			{#key attempt}
				<AppWindow
					{focus}
					mark={shape.mark}
					playback={pane.playback}
					store={pane.store}
					bind:status
					bind:broken
				/>
			{/key}
		</div>
		<button
			type="button"
			disabled={!pane.playback.frames}
			aria-label="Play or pause. Left and right arrows step one frame."
			onclick={pane.playback.toggle}
			class="absolute inset-0 cursor-pointer focus-visible:-outline-offset-2 disabled:cursor-default"
		></button>
		{#if focus.view === "difference"}
			<Chip class="absolute start-2 top-2">Difference</Chip>
		{/if}
		{#if focus.tag}
			<Chip class="absolute end-2 top-2">{focus.tag(pane.playback.frame)}</Chip>
		{/if}
		{#if problem}
			<div
				class="absolute inset-0 grid place-content-center justify-items-center gap-2 bg-void/80 text-sm"
			>
				<p class="text-muted">The preview did not load.</p>
				<button
					type="button"
					onclick={retry}
					class="cursor-pointer rounded-sm border border-seam px-3 py-1 hover:border-muted"
				>
					Try again
				</button>
			</div>
		{:else if waiting}
			<p class="late absolute inset-0 grid place-items-center text-sm text-faint">
				<span class="motion-safe:animate-pulse">Loading</span>
			</p>
		{/if}
	</div>
	<PlaybackBar playback={pane.playback} />
</div>
