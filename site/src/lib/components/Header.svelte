<script lang="ts">
	import { asset, resolve } from "$app/paths";
	import IconButton from "#lib/components/IconButton.svelte";
	import Search from "#lib/components/Search.svelte";
	import { isDark } from "#lib/theme.ts";

	const setTheme = () => {
		const theme = isDark() ? "light" : "dark";
		document.documentElement.dataset.theme = theme;
		try {
			localStorage.setItem("theme", theme);
		} catch (error) {
			console.error(error);
		}
	};

	const toggleTheme = () => {
		if ("startViewTransition" in document) document.startViewTransition(setTheme);
		else setTheme();
	};
</script>

<header
	class="sticky top-0 z-20 flex h-(--header) items-center gap-2 border-b border-seam bg-void px-4 md:px-6"
>
	<span class="md:hidden">
		<IconButton icon="menu" label="Menu" popovertarget="menu" />
	</span>
	<a
		href={resolve("/")}
		class="flex cursor-pointer items-center gap-2 font-semibold pointer-coarse:py-2"
	>
		<img src={asset("logo.svg")} alt="" class="size-6" />
		interpolini
	</a>
	<nav class="ms-auto flex items-center gap-3 text-sm">
		<Search />
		<a
			href="https://github.com/Z1xus/interpolini"
			class="cursor-pointer text-muted hover:text-ink pointer-coarse:py-3">GitHub</a
		>
		<IconButton icon="contrast" label="Switch theme" onclick={toggleTheme} />
	</nav>
</header>
