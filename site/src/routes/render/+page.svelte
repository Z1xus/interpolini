<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import Term from "#lib/components/Term.svelte";
	import { on } from "#lib/window.ts";
</script>

<Section id="render-button" focus={on("rendering", "foot", "go")}>
	<p>
		Render makes one file from the whole timeline. The file goes next to the first video clip as
		<code>name - interpolini.mp4</code>, or to the <Term id="render-folder" />.
	</p>
	<p>After a render, Copy file puts the file on the clipboard.</p>
</Section>

<Section id="render-separate" focus={on("separate-menu", "menu", "file-separate")}>
	<p>
		With File &gt; Export each clip separately on, Render makes one file for each video clip, named
		after that clip. These files have no fades and no images.
	</p>
</Section>

<Section id="render-lossless" focus={on("lossless", "copy", "mode")}>
	<p>
		Lossless cut at the top of the panel copies each clip to its own file with no new encode. Cuts
		go to the nearest keyframe. Not kept lists what a copy loses, such as fades or speed.
	</p>
</Section>

<Section id="render-folder" focus={on("general-settings", "dialog", "folder")}>
	<p>
		Render folder in Settings &gt; General sets where files go. The default is the folder of the
		source clip.
	</p>
</Section>
