<script lang="ts">
	import CodeBlock from "#lib/components/CodeBlock.svelte";
	import Section from "#lib/components/Section.svelte";
	import Term from "#lib/components/Term.svelte";
	import { tile } from "#lib/show.ts";
	import { regions } from "#lib/window.ts";

	const github = "https://github.com/Z1xus/interpolini";
	const window = { rect: regions.window, show: tile("overview", "result") };

	const icon = `mkdir -p ~/.local/share/icons/hicolor/scalable/apps
curl -fsSLo ~/.local/share/icons/hicolor/scalable/apps/interpolini.svg https://raw.githubusercontent.com/Z1xus/interpolini/main/assets/interpolini.svg`;

	const entry = `[Desktop Entry]
Type=Application
Name=Interpolini
Exec=/path/to/interpolini/interpolini
Icon=interpolini
Categories=AudioVideo;Video;
StartupWMClass=interpolini`;
</script>

<Section id="trust" focus={window}>
	<p>
		interpolini is <a href={github} target="_blank" rel="noreferrer">open source</a>, and every
		release is built from source in
		<a
			href="https://docs.github.com/en/actions/get-started/understand-github-actions"
			target="_blank"
			rel="noreferrer">GitHub Actions</a
		>, where
		<a href="{github}/actions/workflows/release.yml" target="_blank" rel="noreferrer"
			>everyone can see the build</a
		>.
	</p>
	<p>
		Releases are
		<a
			href="https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases"
			target="_blank"
			rel="noreferrer">immutable</a
		>, so nobody can change the files of a release after it is out. GitHub also attests each
		release, which lets you
		<a
			href="https://docs.github.com/en/code-security/how-tos/secure-your-supply-chain/secure-your-dependencies/verify-release-integrity"
			target="_blank"
			rel="noreferrer">verify</a
		> that your download is the file from the release.
	</p>
</Section>

<Section id="send-to" focus={{ rect: regions.window }}>
	<p>
		The Windows installer has a Send to option. It adds <code>interpolini</code> and
		<code>interpolini GUI</code> to the Send to menu of Explorer. Right-click a clip and choose
		interpolini to render it with the <Term id="cli-usage">CLI</Term>, or interpolini GUI to open it
		in the window.
	</p>
</Section>

<Section id="linux-menu" focus={window}>
	<p>
		The portable build does not add itself to the app menu. To add it yourself, download the icon
		first.
	</p>
	<CodeBlock code={icon} lang="shell" />
	<p>
		Then save this as <code>~/.local/share/applications/interpolini.desktop</code> and point the Exec
		line at the folder where you unzipped interpolini. Keep the file name, because the taskbar needs it
		to show the icon.
	</p>
	<CodeBlock code={entry} lang="ini" />
</Section>
