<script lang="ts">
	import Icon from "#lib/components/Icon.svelte";
	import Section from "#lib/components/Section.svelte";
	import Term from "#lib/components/Term.svelte";
	import { tile, type Control } from "#lib/show.ts";
	import { details, regions } from "#lib/window.ts";

	const github = "https://github.com/Z1xus/interpolini";
	const releases = `${github}/releases/latest/download`;
	const portable = [
		{ label: "Windows", file: "interpolini-windows-x86_64.zip" },
		{ label: "macOS (Apple Silicon)", file: "interpolini-macos-arm64.zip" },
		{ label: "macOS (Intel)", file: "interpolini-macos-x86_64.zip" },
		{ label: "Linux", file: "interpolini-linux-x86_64.zip" }
	];
	const button =
		"flex cursor-pointer items-center gap-2 rounded-sm border border-accent bg-accent/15 px-3 py-2 text-sm hover:bg-accent/25";
	const blur: Control = {
		kind: "choose",
		label: "Motion blur",
		initial: "After",
		options: [
			{ label: "Before", show: tile("clip", "original") },
			{ label: "After", show: tile("clip", "result") }
		]
	};
	const window = { rect: regions.window, show: tile("overview", "result") };
</script>

<Section
	id="overview"
	focus={{ rect: details.picture, show: tile("clip", "result"), frame: 16 }}
	control={blur}
>
	<p>
		interpolini adds motion blur to your videos by interpolating a clip to a high frame rate and
		blending it back down. See <Term id="interpolation">Concepts</Term> if you want to learn more.
	</p>
</Section>

<Section id="download" focus={window}>
	<p>To use interpolini, download the build for your system and unzip it into any folder.</p>
	<ul class="mt-4 grid gap-2 sm:grid-cols-2">
		{#each portable as { label, file } (file)}
			<li>
				<a href="{releases}/{file}" class={button}>
					<Icon name="download" class="size-4" />
					{label}
				</a>
			</li>
		{/each}
	</ul>
	<p>
		On Windows you can use the installer instead. It can add a
		<Term id="send-to">Send to</Term> shortcut, so you can open a clip in interpolini or start a render
		without leaving Explorer.
	</p>
	<div class="mt-4">
		<a href="{releases}/interpolini-windows-x86_64-setup.exe" class="{button} w-fit">
			<Icon name="download" class="size-4" />
			Windows installer
		</a>
	</div>
	<p>
		These are the official builds from <a href={github} target="_blank" rel="noreferrer">GitHub</a>.
		If you're deciding whether to trust them, read <Term id="trust" />.
	</p>
	<p>
		The only other place to get interpolini is the
		<a href="https://codeberg.org/Z1xus/interpolini" target="_blank" rel="noreferrer"
			>Codeberg mirror</a
		>. Don't download it from anywhere else. If you did, I'm in no way responsible, and you should
		treat that computer as compromised: reinstall the OS and change your passwords. As
		<a
			href="https://learn.microsoft.com/en-us/previous-versions/cc722487(v=technet.10)"
			target="_blank"
			rel="noreferrer">Microsoft's first law of security</a
		> puts it, "If a bad guy can persuade you to run his program on your computer, it's not your computer
		anymore."
	</p>
	<p>On Linux, you can also <Term id="linux-menu">add interpolini to the app menu</Term>.</p>
	<p>
		When a new version is out, an Update button appears under Render. You can turn this check off in
		Settings &gt; General.
	</p>
</Section>
