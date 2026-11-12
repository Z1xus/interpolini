<script lang="ts">
	let { rows }: { rows: readonly (readonly [string, ...string[]])[] } = $props();
</script>

<dl class="my-4 divide-y divide-seam border-y border-seam text-sm">
	{#each rows as [action, ...keys] (action)}
		<div class="flex items-baseline justify-between gap-4 py-1.5">
			<dt class="text-muted">{action}</dt>
			<dd class="flex flex-wrap justify-end gap-1">
				{#each keys as key (key)}
					<kbd class="rounded-sm bg-raised px-1.5 font-mono text-xs text-ink">{key}</kbd>
				{/each}
			</dd>
		</div>
	{/each}
</dl>
