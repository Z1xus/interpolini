<script lang="ts">
	import Icon from "#lib/components/Icon.svelte";
	import type { Snippet } from "svelte";

	let { href, children }: { href: string; children: Snippet } = $props();
</script>

<aside class="my-6">
	<a
		{href}
		target="_blank"
		rel="noreferrer"
		class="flex cursor-pointer items-center gap-3 rounded-md border border-seam bg-void px-3 py-2 text-sm text-muted hover:border-muted hover:text-ink"
	>
		<span class="flex-1">{@render children()}</span>
		<Icon name="external" class="size-4 shrink-0" />
	</a>
</aside>
