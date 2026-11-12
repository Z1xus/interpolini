<script lang="ts">
	import Icon, { type IconName } from "#lib/components/Icon.svelte";
	import type { HTMLButtonAttributes } from "svelte/elements";

	let { icon, label, ...rest }: HTMLButtonAttributes & { icon: IconName; label: string } = $props();
</script>

<button
	type="button"
	aria-label={label}
	title={label}
	{...rest}
	class="grid size-8 cursor-pointer pointer-coarse:size-10 place-items-center rounded-sm text-muted hover:bg-raised hover:text-ink disabled:cursor-default"
>
	<Icon name={icon} class="size-5" />
</button>
