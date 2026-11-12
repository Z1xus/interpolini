<script lang="ts">
	import { moment } from "#lib/assets.ts";
	import FrameStrip from "#lib/components/FrameStrip.svelte";
	import Segment from "#lib/components/Segment.svelte";
	import Slider from "#lib/components/Slider.svelte";
	import { usePane } from "#lib/pane.svelte.ts";
	import type { Control } from "#lib/show.ts";
	import { required } from "#lib/required.ts";
	import type { Attachment } from "svelte/attachments";

	let {
		control,
		selected = $bindable(),
		onchange
	}: { control: Control; selected: number; onchange: () => void } = $props();
	const pane = usePane();
	const labels = $derived(control.kind === "choose" ? control.options.map((o) => o.label) : []);

	const thumbnail =
		(frame: number): Attachment<HTMLCanvasElement> =>
		(canvas) => {
			const entry = pane.store.get("moment");
			if (entry?.status !== "ready" || !(entry.resource instanceof ImageBitmap)) return;
			const [width, height] = [moment.width, moment.height];
			const column = frame % moment.columns;
			const row = Math.floor(frame / moment.columns);
			required(canvas.getContext("2d")).drawImage(
				entry.resource,
				column * width,
				row * height,
				width,
				height,
				0,
				0,
				canvas.width,
				canvas.height
			);
		};

	const pick = (frame: number) => {
		selected = frame;
		onchange();
		pane.playback.stop();
		pane.playback.frame = frame;
	};
</script>

{#snippet tile(frame: number)}
	<canvas width="192" height="108" {@attach thumbnail(frame)} class="size-full"></canvas>
{/snippet}

{#if control.kind === "range"}
	<Slider
		label={control.label}
		min={control.min}
		max={control.max}
		step={control.step}
		{onchange}
		bind:value={selected}
	/>
{:else if control.kind === "choose"}
	<Segment
		label={control.label}
		options={labels}
		{onchange}
		bind:value={() => labels[selected], (label) => (selected = labels.indexOf(label))}
	/>
{:else}
	<FrameStrip
		label={control.label}
		from={control.from}
		to={control.to}
		value={pane.playback.frame}
		real={(frame) => frame % moment.real === 0}
		{tile}
		onselect={pick}
	/>
{/if}
