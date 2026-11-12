<script lang="ts">
	import Note from "#lib/components/Note.svelte";
	import Section from "#lib/components/Section.svelte";
	import { at, on, regions } from "#lib/window.ts";
</script>

<Section id="elsewhere-recording" focus={on("plain", "editor-joint", "import")}>
	<p>
		<a href="https://obsproject.com" target="_blank" rel="noreferrer">OBS</a> is the usual choice
		for recording, because you can tune almost everything about how it captures and encodes your
		game. On Linux there's also
		<a href="https://git.dec05eba.com/gpu-screen-recorder/about/" target="_blank" rel="noreferrer"
			>GPU Screen Recorder</a
		>, which works like ShadowPlay and can save the last few minutes you played.
	</p>
	<Note href="https://ctt.cx/video/obs/">
		If you're interested in recording, ctt.cx has good guides on it.
	</Note>
</Section>

<Section id="elsewhere-sharing" focus={{ rect: regions.render, mark: regions.render }}>
	<Note href="https://ctt.cx/misc/filesharing/">
		ctt.cx lists file hosts that do not compress your video.
	</Note>
</Section>

<Section id="elsewhere-youtube" focus={at("upscale", "resolution")}>
	<Note href="https://support.google.com/youtube/answer/1722171">
		Read the upload settings that YouTube recommends.
	</Note>
</Section>

<Section id="elsewhere-ffmpeg" focus={on("components", "dialog", "ffmpeg")}>
	<Note href="https://ffmpeg.org">Get other FFmpeg builds at ffmpeg.org.</Note>
	<Note href="https://trac.ffmpeg.org/wiki/Encode/H.264">
		The FFmpeg wiki explains codecs, quality and presets.
	</Note>
</Section>
