<script lang="ts">
	const repository = "https://github.com/Z1xus/interpolini";
	const link = "cursor-pointer hover:text-ink pointer-coarse:py-3";
</script>

<footer
	class="flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-seam px-4 py-6 text-xs text-faint md:px-10"
>
	<a href="{repository}/blob/main/LICENSE" class={link}>License</a>
	<span class="ms-auto">
		Built from
		<a href="{repository}/commit/{COMMIT}" class="{link} font-mono">{COMMIT.slice(0, 12)}</a>
	</span>
</footer>
