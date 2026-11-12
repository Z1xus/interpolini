<script lang="ts" generics="T extends string | number">
	let {
		label,
		options,
		value = $bindable(),
		onchange
	}: { label: string; options: readonly T[]; value: T; onchange?: () => void } = $props();
	const name = $props.id();
</script>

<fieldset class="flex min-w-0 flex-wrap gap-1">
	<legend class="sr-only">{label}</legend>
	{#each options as option (option)}
		<label
			class="cursor-pointer rounded-sm border border-seam px-2.5 py-1 text-sm pointer-coarse:py-2.5 text-muted hover:border-muted has-checked:border-accent has-checked:bg-accent/15 has-checked:text-ink has-focus-visible:outline-2 has-focus-visible:outline-accent"
		>
			<input
				type="radio"
				{name}
				checked={option === value}
				onclick={() => {
					if (option === value) onchange?.();
				}}
				onchange={() => {
					value = option;
					onchange?.();
				}}
				class="sr-only cursor-pointer"
			/>
			{option}
		</label>
	{/each}
</fieldset>
