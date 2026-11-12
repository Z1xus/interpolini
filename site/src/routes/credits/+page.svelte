<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import { tile } from "#lib/show.ts";
	import { regions } from "#lib/window.ts";

	const window = { rect: regions.window, show: tile("overview", "result") };
</script>

{#snippet out(href: string, text: string)}
	<a {href} target="_blank" rel="noreferrer">{text}</a>
{/snippet}

<Section id="credits-author" focus={window}>
	<p>interpolini is made by {@render out("https://z1x.us/", "z1xus")}.</p>
</Section>

<Section id="credits-thanks" focus={window}>
	<p>
		None of this would have been possible without the projects and people below, so thank you, all
		of you!
	</p>
	<p>
		{@render out("https://github.com/couleur-tweak-tips/smoothie-rs", "Smoothie")} is where interpolini
		comes from. It grew out of an attempt to run Smoothie in a browser, and it still does the same job
		today, so a huge thanks to Couleur and everyone who worked on it.
	</p>
	<p>
		{@render out("https://ctt.cx", "ctt.cx")} is the original place to learn all of this, with guides
		on recording, codecs, file sharing and Smoothie itself, which is why these docs keep linking to it.
	</p>
	<p>
		{@render out("https://blur.sh", "blur")} by f0e is another great way to add motion blur, and its
		{@render out(
			"https://github.com/f0e/blur#recommended-settings-for-gameplay-footage",
			"recommended settings"
		)} are worth a read even if you use interpolini.
	</p>
	<p>
		And thank you {@render out("https://www.youtube.com/@omnipathic", "Omnipathic")} for the clips you
		see all over these docs!
	</p>
</Section>

<Section id="credits-projects" focus={window}>
	<p>
		interpolini is built on a lot of other people's work:
		{@render out("https://github.com/hzwer/ECCV2022-RIFE", "RIFE")} by hzwer and its
		{@render out("https://github.com/TNTwise/rife-ncnn-vulkan", "ncnn port")} by nihui and TNTwise,
		{@render out("https://www.svp-team.com/wiki/Plugins:_SVPflow", "SVPflow")} by the SVP team, the original
		behind {@render out("https://github.com/Z1xus/open-svpflow", "open-svpflow")},
		{@render out("https://ffmpeg.org", "FFmpeg")} for decoding and encoding, and
		{@render out("https://slint.dev", "Slint")} for the window.
	</p>
	<p>
		Thank you to everyone in the community around these projects too. interpolini wouldn't be here
		without you.
	</p>
</Section>
