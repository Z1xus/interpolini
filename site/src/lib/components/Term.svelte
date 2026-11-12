<script lang="ts">
	import { locate, type SectionId } from "#lib/pages.ts";
	import type { Snippet } from "svelte";

	let { id, children }: { id: SectionId; children?: Snippet } = $props();
	const target = $derived(locate(id));
</script>

<a
	href={target.href}
	class="cursor-pointer text-ink underline decoration-seam underline-offset-[0.2em] hover:decoration-ink"
>
	{#if children}
		{@render children()}
	{:else}
		{target.title}
	{/if}
</a>
