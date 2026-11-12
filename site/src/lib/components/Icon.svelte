<script lang="ts" module>
	export type IconName =
		| "menu"
		| "contrast"
		| "download"
		| "external"
		| "play"
		| "pause"
		| "back"
		| "forward"
		| "link"
		| "check"
		| "copy"
		| "search";
</script>

<script lang="ts">
	let { name, class: className }: { name: IconName; class?: string } = $props();
</script>

<svg
	viewBox="0 0 24 24"
	fill="none"
	stroke="currentColor"
	stroke-width="1.75"
	stroke-linecap="round"
	stroke-linejoin="round"
	aria-hidden="true"
	class={className}
>
	{#if name === "menu"}
		<path d="M4 7h16M4 12h16M4 17h16" />
	{:else if name === "contrast"}
		<circle cx="12" cy="12" r="9" />
		<path d="M12 3a9 9 0 0 1 0 18z" fill="currentColor" />
	{:else if name === "download"}
		<path d="M12 4v11m0 0-4-4m4 4 4-4M5 20h14" />
	{:else if name === "play"}
		<path d="M8 5.5v13l10-6.5z" fill="currentColor" />
	{:else if name === "pause"}
		<path d="M8.5 5.5v13M15.5 5.5v13" stroke-width="3" />
	{:else if name === "back"}
		<path d="M6.5 5.5v13" />
		<path d="M18 6v12l-8-6z" fill="currentColor" />
	{:else if name === "forward"}
		<path d="M17.5 5.5v13" />
		<path d="M6 6v12l8-6z" fill="currentColor" />
	{:else if name === "link"}
		<path
			d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"
		/>
	{:else if name === "check"}
		<path d="M5 12.5l4.5 4.5L19 7.5" />
	{:else if name === "copy"}
		<rect x="9" y="9" width="11" height="11" rx="2" />
		<path d="M15 5.5A1.5 1.5 0 0 0 13.5 4h-8A1.5 1.5 0 0 0 4 5.5v8A1.5 1.5 0 0 0 5.5 15" />
	{:else if name === "search"}
		<circle cx="11" cy="11" r="6.5" />
		<path d="m16 16 4.5 4.5" />
	{:else}
		<path d="M8 16 17 7m0 0H9m8 0v8" />
	{/if}
</svg>
