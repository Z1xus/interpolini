<script lang="ts">
	let { name, range, initial }: { name: string; range: string; initial: string } = $props();
</script>

<dl class="my-3 flex flex-wrap gap-x-6 gap-y-1 font-mono text-xs text-faint">
	<div class="flex gap-2">
		<dt class="sr-only">Name</dt>
		<dd class="text-ink">{name}</dd>
	</div>
	<div class="flex gap-2">
		<dt>range</dt>
		<dd class="text-ink">{range}</dd>
	</div>
	<div class="flex gap-2">
		<dt>default</dt>
		<dd class="text-ink">{initial}</dd>
	</div>
</dl>
