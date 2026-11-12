<script lang="ts">
	import type { Snippet } from "svelte";

	let {
		label,
		from,
		to,
		value,
		real,
		tile,
		onselect
	}: {
		label: string;
		from: number;
		to: number;
		value: number;
		real: (frame: number) => boolean;
		tile: Snippet<[number]>;
		onselect: (frame: number) => void;
	} = $props();

	const frames = $derived(Array.from({ length: to - from + 1 }, (_, index) => from + index));
	let rail: HTMLDivElement;
	const buttons: HTMLButtonElement[] = [];

	$effect(() => {
		const button = buttons[value - from];
		if (button && rail)
			rail.scrollLeft = button.offsetLeft - (rail.clientWidth - button.offsetWidth) / 2;
	});

	const press = (event: KeyboardEvent) => {
		let next = value;
		if (event.key === "ArrowLeft") next--;
		else if (event.key === "ArrowRight") next++;
		else if (event.key === "Home") next = from;
		else if (event.key === "End") next = to;
		else return;
		event.preventDefault();
		next = Math.max(from, Math.min(to, next));
		onselect(next);
		buttons[next - from].focus({ preventScroll: true });
	};
</script>

<div
	role="group"
	aria-label={label}
	bind:this={rail}
	class="relative flex min-w-0 items-end gap-2 overflow-x-auto overscroll-x-contain border-y border-seam bg-void/40 px-[calc(50%-3.5rem)] pt-3 pb-2 [scrollbar-width:thin]"
>
	{#each frames as frame (frame)}
		<button
			type="button"
			aria-pressed={frame === value}
			aria-label="Frame {frame + 1}, {real(frame) ? 'recorded' : 'interpolated'}"
			tabindex={frame === value ? 0 : -1}
			bind:this={buttons[frame - from]}
			onclick={() => onselect(frame)}
			onkeydown={press}
			class={[
				"group relative shrink-0 cursor-pointer pb-4",
				real(frame) ? "w-28" : "w-20",
				frame === value ? "text-accent" : "text-faint"
			]}
		>
			<span
				class={[
					"block aspect-video overflow-hidden rounded-sm border transition-[opacity,filter] group-hover:opacity-100 group-hover:grayscale-0",
					!real(frame) && "border-dashed",
					frame === value ? "border-accent" : real(frame) ? "border-muted" : "border-seam",
					frame !== value && "opacity-60 grayscale-50"
				]}
			>
				{@render tile(frame)}
			</span>
			<span class="mt-1 grid justify-items-center font-mono text-[10px] leading-3.5">
				<span>{frame + 1}</span>
				<span>{real(frame) ? "recorded" : "interpolated"}</span>
			</span>
			{#if frame === value}
				<span aria-hidden="true" class="absolute start-1/2 bottom-0 h-3 w-px bg-accent"></span>
			{/if}
		</button>
	{/each}
</div>
