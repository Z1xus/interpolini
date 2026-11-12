<script lang="ts">
	import IconButton from "#lib/components/IconButton.svelte";
	import Segment from "#lib/components/Segment.svelte";
	import { speeds, type Playback } from "#lib/playback.svelte.ts";

	let { playback }: { playback: Playback } = $props();

	const labels = ["1x", "1/4x", "1/8x"] as const;
</script>

<div
	role="toolbar"
	aria-label="Playback"
	inert={!playback.frames}
	class={["flex h-(--bar) items-center gap-1 px-2", !playback.frames && "invisible"]}
>
	<IconButton icon="back" label="Previous frame" onclick={() => playback.step(-1)} />
	<IconButton
		icon={playback.playing ? "pause" : "play"}
		label={playback.playing ? "Pause" : "Play"}
		onclick={playback.toggle}
	/>
	<IconButton icon="forward" label="Next frame" onclick={() => playback.step(1)} />
	<div class="ms-2">
		<Segment
			label="Speed"
			options={labels}
			bind:value={
				() => labels[speeds.indexOf(playback.speed as (typeof speeds)[number])],
				(label) => (playback.speed = speeds[labels.indexOf(label)])
			}
		/>
	</div>
	<output class="ms-auto font-mono text-xs text-faint">
		{playback.frame + 1} / {playback.frames}
	</output>
</div>
