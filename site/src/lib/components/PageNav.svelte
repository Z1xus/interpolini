<script lang="ts">
	import { resolve } from "$app/paths";
	import { route, type Page } from "#lib/pages.ts";

	let { previous, next }: { previous?: Page; next?: Page } = $props();
</script>

<nav
	aria-label="Previous and next"
	class="mt-16 grid gap-3 border-t border-seam pt-6 sm:grid-cols-2"
>
	{#if previous}
		<a
			href={resolve(route(previous))}
			class="cursor-pointer rounded-md border border-seam px-4 py-3 hover:border-muted"
		>
			<span class="block text-xs text-faint">Previous</span>
			<span class="font-medium">{previous.title}</span>
		</a>
	{/if}
	{#if next}
		<a
			href={resolve(route(next))}
			class="cursor-pointer rounded-md border border-seam px-4 py-3 text-end hover:border-muted sm:col-start-2"
		>
			<span class="block text-xs text-faint">Next</span>
			<span class="font-medium">{next.title}</span>
		</a>
	{/if}
</nav>
