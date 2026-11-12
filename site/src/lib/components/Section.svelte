<script lang="ts">
	import { record } from "#lib/audit.ts";
	import type { AssetName } from "#lib/assets.ts";
	import Controls from "#lib/components/Control.svelte";
	import Icon from "#lib/components/Icon.svelte";
	import type { Focus } from "#lib/focus.ts";
	import { locate, type SectionId } from "#lib/pages.ts";
	import { usePane } from "#lib/pane.svelte.ts";
	import { assetOf, initialOf, keyFrameOf, labelOf, selectedOf, type Control } from "#lib/show.ts";
	import { hashOf, writeHash } from "#lib/url.ts";
	import type { Snippet } from "svelte";

	let {
		id,
		focus,
		control,
		children
	}: { id: SectionId; focus: Focus; control?: Control; children?: Snippet } = $props();

	const pane = usePane();
	const title = $derived.by(() => {
		record(id);
		return locate(id).title;
	});
	const start = () => focus.frame ?? keyFrameOf(focus.show);
	const initial = () => initialOf(control, focus.show, start());
	let selected = $state(initial());
	let copied = $state(false);

	const current = $derived.by<Focus>(() => {
		const show = focus.show;
		if (control?.kind === "choose") {
			const option = control.options[selected];
			return {
				...focus,
				...option.spot,
				show: option.show,
				view: option.view ?? focus.view,
				scene: option.scene ?? focus.scene
			};
		}
		if (control?.kind === "range" && show && "blend" in show) {
			return { ...focus, show: { blend: { ...show.blend, [control.field]: selected } } };
		}
		return focus;
	});

	const assets = (): AssetName[] => {
		const shows = control?.kind === "choose" ? control.options.map((o) => o.show) : [focus.show];
		return [...new Set(shows.flatMap((show) => (show ? [assetOf(show)] : [])))];
	};

	const frame = () =>
		control?.kind === "frames" ? selected : (current.frame ?? keyFrameOf(current.show));

	const linked = () => (control && selected !== initial() ? labelOf(control, selected) : undefined);

	const select = (text?: string) => {
		const value = control && text !== undefined ? selectedOf(control, text) : undefined;
		selected = value ?? initial();
		if (control?.kind === "frames") {
			pane.playback.stop();
			pane.playback.frame = selected;
		}
	};

	const changed = () => {
		pane.activate(id);
		writeHash(id, linked());
	};

	const copy = () => {
		const link = new URL(hashOf(id, linked()), location.href);
		navigator.clipboard
			.writeText(link.href)
			.then(() => {
				copied = true;
				setTimeout(() => (copied = false), 1600);
			})
			.catch(console.error);
	};

	const active = $derived(pane.active === id);

	$effect(() => {
		if (active) pane.focus = current;
	});
</script>

<section
	{id}
	data-media={assets().join(" ")}
	{@attach pane.track({ id, assets, select, frame })}
	class="relative"
>
	<span
		aria-hidden="true"
		class={[
			"pointer-events-none absolute inset-y-0 -start-3 w-[3px] origin-top rounded-full bg-accent transition-transform duration-300",
			active ? "scale-y-100" : "scale-y-0"
		]}
	></span>
	<div class="group mb-3 flex items-center gap-1">
		<h2
			class={[
				"transition-colors duration-300",
				active ? "font-bold text-ink" : "font-medium text-muted"
			]}
		>
			{title}
		</h2>
		<button
			type="button"
			onclick={copy}
			aria-label="Copy link to {title}"
			class="grid size-6 cursor-pointer pointer-coarse:-my-2 pointer-coarse:size-10 place-items-center rounded-sm text-faint hover:text-ink focus-visible:opacity-100 pointer-fine:opacity-0 pointer-fine:group-hover:opacity-100"
		>
			<Icon name={copied ? "check" : "link"} class="size-4" />
		</button>
		<span class="sr-only" aria-live="polite">{copied ? "Link copied" : ""}</span>
	</div>
	{@render children?.()}
	{#if control}
		<div class="mt-4">
			<Controls {control} bind:selected onchange={changed} />
		</div>
	{/if}
</section>
