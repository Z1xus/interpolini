<script lang="ts">
	import Icon from "#lib/components/Icon.svelte";

	const rules = {
		shell: /(?<ink>^\S+)|(?<accent>(?<=\s)--?[\w-]+)/gm,
		ini: /(?<accent>^\[.*\]$)|(?<ink>^[^:=\n]+(?=[:=]))/gm,
		json: /(?<ink>"[^"]*"(?=:))|(?<faint>[{}:,])/g
	};
	const tones: Record<string, string> = {
		ink: "text-ink",
		accent: "text-accent",
		faint: "text-faint"
	};

	let { code, lang }: { code: string; lang: keyof typeof rules } = $props();
	let copied = $state(false);

	const parts = $derived.by(() => {
		const list: { text: string; tone?: string }[] = [];
		let end = 0;
		for (const match of code.matchAll(rules[lang])) {
			const [tone] = Object.entries(match.groups ?? {}).find(([, text]) => text) ?? [];
			list.push({ text: code.slice(end, match.index) }, { text: match[0], tone });
			end = match.index + match[0].length;
		}
		list.push({ text: code.slice(end) });
		return list;
	});

	const copy = () => {
		navigator.clipboard
			.writeText(code)
			.then(() => {
				copied = true;
				setTimeout(() => (copied = false), 1600);
			})
			.catch(console.error);
	};
</script>

<div class="relative my-4 overflow-hidden rounded-md border border-seam bg-void">
	<pre class="overflow-x-auto p-3 font-mono text-xs leading-relaxed text-muted"><code
			class="inline-block bg-transparent p-0 pe-8"
			>{#each parts as part, index (index)}<span class={part.tone && tones[part.tone]}
					>{part.text}</span
				>{/each}</code
		></pre>
	<div
		class="pointer-events-none absolute end-0 top-0 bottom-3 w-16 bg-linear-to-l from-void from-50% to-transparent"
	></div>
	<button
		type="button"
		onclick={copy}
		aria-label={copied ? "Copied" : "Copy"}
		class={[
			"absolute end-0 top-0 box-content flex h-lh cursor-pointer items-center p-3 text-xs leading-relaxed",
			copied ? "text-accent" : "text-faint hover:text-ink"
		]}
	>
		<Icon name={copied ? "check" : "copy"} class="size-3.5" />
	</button>
</div>
