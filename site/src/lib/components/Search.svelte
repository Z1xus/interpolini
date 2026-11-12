<script lang="ts">
	import { afterNavigate, goto } from "$app/navigation";
	import { resolve } from "$app/paths";
	import Icon from "#lib/components/Icon.svelte";
	import type { Entry, searcher } from "#lib/search.ts";

	type Reply = { ready: boolean } | { text: string; vector: Float32Array };

	let dialog: HTMLDialogElement;
	let worker: Worker | undefined;
	let text = $state("");
	let chosen = $state(0);
	let keys = $state("Ctrl K");
	let rank = $state<ReturnType<typeof searcher>>();
	let model = $state<"loading" | "ready" | "failed">("loading");
	let meaning = $state.raw<{ text: string; vector: Float32Array }>();
	let busy = false;
	let index: Promise<void> | undefined;

	const wanted = $derived(text.trim());
	const results = $derived.by(() => {
		if (!rank || !wanted) return [];
		if (model !== "ready") return rank(wanted);
		return meaning ? rank(meaning.text, meaning.vector) : [];
	});
	const current = $derived(model !== "ready" || meaning?.text === wanted);
	const hrefOf = (entry: Entry) => `${resolve("/")}${entry.path}`;
	const placeOf = ({ group, page, title }: Entry) =>
		page === title ? group : `${group} › ${page}`;

	const ask = () => {
		if (busy || model !== "ready" || !wanted || meaning?.text === wanted) return;
		busy = true;
		worker?.postMessage({ text: wanted });
	};

	function load() {
		if (!worker) {
			worker = new Worker(new URL("../search.worker.ts", import.meta.url), { type: "module" });
			worker.onmessage = ({ data }: MessageEvent<Reply>) => {
				if ("ready" in data) model = data.ready ? "ready" : "failed";
				else {
					busy = false;
					meaning = data;
				}
				ask();
			};
			worker.postMessage({ root: new URL(`${resolve("/")}model/`, location.href).href });
		}
		index ??= Promise.all([
			import("#lib/search.ts"),
			fetch(`${resolve("/")}search.json`).then((response) => response.json())
		])
			.then(([{ searcher }, entries]) => void (rank = searcher(entries as Entry[])))
			.catch((error: unknown) => {
				console.error(error);
				index = undefined;
			});
	}

	function open() {
		load();
		text = "";
		dialog.showModal();
	}

	$effect(() => {
		if (/Mac|iPhone|iPad/.test(navigator.platform)) keys = "⌘ K";
	});

	$effect(() => {
		void wanted;
		chosen = 0;
		ask();
	});

	afterNavigate(() => dialog.close());

	const shortcut = (event: KeyboardEvent) => {
		if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== "k") return;
		event.preventDefault();
		if (dialog.open) dialog.close();
		else open();
	};

	const move = (event: KeyboardEvent) => {
		if (event.key === "ArrowDown") chosen = Math.min(chosen + 1, results.length - 1);
		else if (event.key === "ArrowUp") chosen = Math.max(chosen - 1, 0);
		else if (event.key === "Enter" && current && results[chosen]) goto(hrefOf(results[chosen]));
		else return;
		event.preventDefault();
	};
</script>

<svelte:window onkeydown={shortcut} />

<button
	type="button"
	onclick={open}
	onpointerenter={load}
	aria-label="Search"
	class="flex h-8 cursor-pointer items-center gap-2 rounded-sm border-seam px-2 text-muted hover:text-ink pointer-coarse:h-10 sm:w-52 sm:border sm:bg-rack"
>
	<Icon name="search" class="size-5 sm:size-4" />
	<span class="hidden flex-1 text-start sm:inline">Search</span>
	<kbd class="hidden rounded-sm bg-raised px-1.5 font-mono text-xs sm:inline">{keys}</kbd>
</button>

<dialog
	bind:this={dialog}
	onclick={(event) => event.target === dialog && dialog.close()}
	aria-label="Search"
	class="mx-auto mt-[12svh] w-[min(38rem,calc(100vw-2rem))] rounded-md border border-seam bg-rack p-0 text-ink shadow-2xl backdrop:bg-black/50"
>
	<div class="flex items-center gap-3 px-4">
		<Icon name="search" class="size-5 shrink-0 text-faint" />
		<input
			bind:value={text}
			onkeydown={move}
			type="text"
			role="combobox"
			aria-label="Search the docs"
			aria-controls="search-results"
			aria-expanded={results.length > 0}
			aria-activedescendant={results.length > 0 ? `search-result-${chosen}` : undefined}
			aria-autocomplete="list"
			autocomplete="off"
			spellcheck="false"
			placeholder="Search the docs"
			class="h-12 min-w-0 flex-1 bg-transparent outline-none placeholder:text-faint"
		/>
		<kbd class="rounded-sm bg-raised px-1.5 font-mono text-xs text-muted">Esc</kbd>
	</div>
	<ul
		id="search-results"
		role="listbox"
		aria-label="Results"
		class="max-h-[60svh] overflow-y-auto border-seam empty:hidden [&:not(:empty)]:border-t [&:not(:empty)]:p-2"
	>
		{#each results as entry, index (entry.path)}
			<li id="search-result-{index}" role="option" aria-selected={index === chosen}>
				<a
					href={hrefOf(entry)}
					onpointermove={() => (chosen = index)}
					class={[
						"block cursor-pointer rounded-sm px-3 py-2 transition-none",
						index === chosen && "bg-raised"
					]}
				>
					<span class="flex items-baseline justify-between gap-4">
						<span class="font-medium">{entry.title}</span>
						<span class="shrink-0 text-xs text-faint">{placeOf(entry)}</span>
					</span>
					<span class="block truncate text-sm text-muted">{entry.text}</span>
				</a>
			</li>
		{/each}
	</ul>
	{#if wanted && rank && results.length === 0 && (model !== "ready" || meaning)}
		<p class="border-t border-seam px-4 py-3 text-sm text-muted">No results</p>
	{/if}
	{#if model === "loading"}
		<p class="border-t border-seam px-4 py-2 text-xs text-faint motion-safe:animate-pulse">
			Loading search by meaning…
		</p>
	{/if}
</dialog>
