<script lang="ts">
	import "#lib/app.css";
	import { afterNavigate, beforeNavigate, preloadData } from "$app/navigation";
	import { asset } from "$app/paths";
	import { page } from "$app/state";
	import type { AssetPath } from "$app/types";
	import Footer from "#lib/components/Footer.svelte";
	import Header from "#lib/components/Header.svelte";
	import PageNav from "#lib/components/PageNav.svelte";
	import Pane from "#lib/components/Pane.svelte";
	import Sidebar from "#lib/components/Sidebar.svelte";
	import { providePane } from "#lib/pane.svelte.ts";
	import { origin, pages, route } from "#lib/pages.ts";

	let { children } = $props();
	const pane = providePane();
	beforeNavigate(({ to }) => {
		if (to?.url.pathname !== page.url.pathname) pane.reset();
	});
	afterNavigate(({ shallow }) => {
		if (!shallow) pane.restore(location.hash);
	});

	let last = { x: 0, y: 0, time: 0 };
	let warmed: HTMLAnchorElement | null = null;
	const ahead = ({
		clientX: x,
		clientY: y,
		timeStamp: time,
		target,
		pointerType
	}: PointerEvent) => {
		const pxPerMs = Math.hypot(x - last.x, y - last.y) / Math.max(time - last.time, 1);
		const slow = pxPerMs < 0.3;
		last = { x, y, time };
		const link = target instanceof Element ? target.closest("a") : null;
		if (!link || link === warmed || !slow || pointerType === "touch") return;
		if (link.origin !== location.origin || link.pathname === location.pathname) return;
		warmed = link;
		preloadData(link.href).catch(console.error);
		pane.warm(link).catch(console.error);
	};

	const index = $derived(pages.findIndex((entry) => route(entry) === page.route.id));
	const current = $derived(pages[index]);
	const title = $derived(current.slug ? `${current.title} | interpolini` : current.title);
</script>

<svelte:head>
	<title>{title}</title>
	<meta name="description" content={current.description} />
	<meta name="theme-color" content="#e8483a" />
	<meta property="og:site_name" content="interpolini" />
	<meta property="og:title" content={title} />
	<meta property="og:description" content={current.description} />
	<meta property="og:url" content={new URL(page.url.pathname, origin).href} />
	<meta property="og:image" content={new URL(asset("media/logo.png" as AssetPath), origin).href} />
	<meta name="twitter:card" content="summary" />
</svelte:head>

<svelte:window onhashchange={() => pane.restore(location.hash)} />
<svelte:document onpointermove={ahead} />

<a
	href="#content"
	class="fixed start-2 top-2 z-50 -translate-y-16 cursor-pointer rounded-sm border border-seam bg-void px-3 py-1.5 text-sm font-medium text-ink focus:translate-y-0"
>
	Skip to content
</a>
<Header />
<div class="md:flex">
	<Sidebar />
	<div class="min-w-0 flex-1">
		<main
			{@attach pane.watch}
			class="grid gap-x-10 px-4 lg:grid-cols-[minmax(0,26rem)_minmax(0,1fr)] md:px-10"
		>
			{#key current.slug}
				<article
					id="content"
					tabindex="-1"
					class="arrive min-w-0 pt-8 pb-[max(4rem,calc((100svh-var(--reading-top))*0.65-9rem))] outline-none lg:pt-14"
				>
					<h1 class="text-3xl font-semibold tracking-tight">{current.title}</h1>
					{@render children()}
					<PageNav previous={pages[index - 1]} next={pages[index + 1]} />
				</article>
			{/key}
			<Pane
				class="sticky top-(--header) z-10 order-first -mx-4 md:mx-0 lg:top-20 lg:order-none lg:mt-14 lg:self-start"
			/>
		</main>
		<Footer />
	</div>
</div>
