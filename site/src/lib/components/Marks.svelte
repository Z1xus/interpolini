<script lang="ts">
	import { draw, type Mark } from "#lib/marks.ts";

	let { marks, width, height }: { marks: readonly Mark[]; width: number; height: number } =
		$props();
	const drawn = $derived(marks.map((mark, index) => draw(mark, width, height, index)));
</script>

<svg
	viewBox="0 0 {width} {height}"
	aria-hidden="true"
	class="pointer-events-none absolute inset-0 size-full overflow-visible text-accent"
>
	{#each drawn as item, index (index)}
		{#if "path" in item}
			<path d={item.path} pathLength="1" class="mark" style:--delay="{0.3 + index * 0.25}s" />
		{/if}
	{/each}
</svg>
{#each drawn as item, index (index)}
	{#if "text" in item}
		<span
			aria-hidden="true"
			class={["mark-label", item.centered && "centered"]}
			style:left="{item.x}px"
			style:top="{item.y}px"
			style:--delay="{0.3 + index * 0.25}s"
		>
			{item.text}
		</span>
	{/if}
{/each}
