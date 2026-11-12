<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import Setting from "#lib/components/Setting.svelte";
	import Term from "#lib/components/Term.svelte";
	import { ghostSword, middle } from "#lib/callouts.ts";
	import { tile, variants } from "#lib/show.ts";
	import { at } from "#lib/window.ts";

	const result = tile("output-quality", "20");
</script>

<Section
	id="output-section"
	focus={{
		...at("output", "header"),
		show: result
	}}
>
	<p>
		interpolini encodes the frames after <Term id="upscale-section">upscaling</Term>. The header
		shows the estimated size of the file.
	</p>
</Section>

<Section id="output-codec" focus={{ ...at("output", "codec"), show: result }}>
	<Setting name="output.codec" range="h264, hevc, av1" initial="h264" />
	<p>Sets the video format.</p>
	<table>
		<tbody>
			<tr><th>h264</th><td>plays on almost every device</td></tr>
			<tr>
				<th>hevc</th>
				<td>usually a smaller file at the same quality, but fewer players and sites take it</td>
			</tr>
			<tr>
				<th>av1</th>
				<td
					>usually the smallest file and the slowest encode. Hardware encoding needs a newer GPU.</td
				>
			</tr>
		</tbody>
	</table>
	<p>
		<Term id="elsewhere-ffmpeg">More about codecs, quality and presets</Term>.
	</p>
</Section>

<Section id="output-encoder" focus={{ ...at("output", "encoder"), show: result }}>
	<Setting name="output.encoder" range="auto, hardware, software" initial="auto" />
	<p>Sets which encoder makes the video.</p>
	<table>
		<tbody>
			<tr>
				<th>auto</th>
				<td>the hardware encoders of the computer first, then a software encoder</td>
			</tr>
			<tr><th>hardware</th><td>only hardware encoders. The render fails when none opens.</td></tr>
			<tr><th>software</th><td>libx264, libx265 or libsvtav1</td></tr>
		</tbody>
	</table>
	<p>
		The hardware encoders are NVENC, AMF, QSV and VAAPI, and Vulkan and VideoToolbox for h264 and
		hevc. Hardware encoders are usually faster. Software encoders usually make a smaller file at the
		same quality.
	</p>
</Section>

<Section id="output-target" focus={{ ...at("output", "target"), show: result }}>
	<Setting name="output.target" range="quality, size" initial="quality" />
	<table>
		<tbody>
			<tr>
				<th>quality</th>
				<td>
					keeps the quality the same and lets the file size change. The panel shows
					<Term id="output-quality" />.
				</td>
			</tr>
			<tr>
				<th>size</th>
				<td>
					keeps the file size near a limit and lets the quality change. The panel shows
					<Term id="output-size" />.
				</td>
			</tr>
		</tbody>
	</table>
</Section>

<Section
	id="output-quality"
	focus={{ rect: middle, panel: "output", show: result, marks: ghostSword }}
	control={variants("output-quality", "Quality", "20")}
>
	<Setting name="output.quality" range="0 to 51" initial="20" />
	<p>
		Sets the quality when the target is quality. A lower number gives better quality and a larger
		file. These examples use the software encoder. At 30, the grass loses detail. At 40, blocks
		replace the blurred sword.
	</p>
	<p>The same number gives other results in other encoders.</p>
</Section>

<Section id="output-size" focus={{ ...at("size", "size"), show: result }}>
	<Setting name="output.size" range="1 to 100000 MB" initial="10 MB" />
	<p>
		Sets the size of the file in MB when the target is size. The file can end a little under this
		size.
	</p>
	<p>
		If the file is larger, interpolini renders again with a lower bitrate, and then with the
		software encoder. If it is still larger, a warning shows. A size too small for the length of the
		clip gives an error.
	</p>
</Section>

<Section id="output-effort" focus={{ ...at("output", "effort"), show: result }}>
	<Setting name="output.effort" range="fastest, fast, balanced, best" initial="fast" />
	<p>
		Sets how much time the encoder spends on each frame. A higher effort usually makes a smaller
		file at the same quality and renders slower. It sets the encoder preset. The tip of the Effort
		row shows the preset of each choice. A preset in <Term id="output-options" /> replaces the effort.
	</p>
</Section>

<Section id="output-audio" focus={{ ...at("output", "audio"), show: result }}>
	<Setting name="output.audio" range="separate, mix" initial="mix" />
	<p>Sets what happens to the audio tracks of a clip, for example game, voice and music.</p>
	<p>
		The mix option gives one track. interpolini removes tracks that repeat others and mixes the
		rest. The separate option copies each track as it is. In a timeline export, separate gives one
		track for each audio track of the timeline.
	</p>
	<p>
		A target size, a clip with another speed and a timeline export encode the sound again to AAC at
		192 kb/s.
	</p>
</Section>

<Section id="output-options" focus={{ ...at("output", "options"), show: result }}>
	<Setting name="output.options" range="text" initial="empty" />
	<p>
		Passes options to the encoder. Write each as a name and a value, for example
		<code>-preset p7 -cq 18</code>. The names are the FFmpeg names of the encoder. The options
		replace the settings that interpolini makes.
	</p>
	<p>
		<code>-c:v</code> with an encoder name, for example <code>-c:v hevc_nvenc</code>, makes
		interpolini use that encoder and skip the others.
	</p>
</Section>

<Section id="output-container" focus={{ ...at("output", "container"), show: result }}>
	<Setting name="output.container" range="mp4, mkv, mov" initial="mp4" />
	<p>Sets the file type of the result.</p>
	<table>
		<tbody>
			<tr><th>mp4</th><td>plays almost everywhere</td></tr>
			<tr><th>mov</th><td>the usual choice for editing programs</td></tr>
			<tr>
				<th>mkv</th>
				<td
					>holds almost any stream. Plays in most desktop players, but in fewer browsers and phones.</td
				>
			</tr>
		</tbody>
	</table>
</Section>
