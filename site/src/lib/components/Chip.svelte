<script lang="ts">
	import type { Snippet } from "svelte";

	let { class: className, children }: { class?: string; children: Snippet } = $props();
</script>

<span class={["rounded-sm bg-black/60 px-1.5 py-0.5 font-mono text-xs text-white", className]}>
	{@render children()}
</span>
