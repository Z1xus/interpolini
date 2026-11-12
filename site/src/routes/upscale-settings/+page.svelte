<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import Setting from "#lib/components/Setting.svelte";
	import Term from "#lib/components/Term.svelte";
	import { middle, stairs } from "#lib/callouts.ts";
	import { tile, variants } from "#lib/show.ts";
	import { at } from "#lib/window.ts";

	const nearest = tile("upscale-method", "nearest");
</script>

<Section id="upscale-section" focus={{ ...at("upscale"), show: nearest }}>
	<Setting name="upscale.enabled" range="yes, no" initial="no" />
	<p>
		interpolini scales each frame after
		<Term id="color-section">color</Term> and before it encodes the frame.
	</p>
	<p>
		Upscaling adds no detail. It is mostly used for <Term id="elsewhere-youtube">YouTube</Term>,
		where a higher resolution gets a higher bitrate and looks less compressed.
	</p>
</Section>

<Section id="upscale-resolution" focus={{ ...at("upscale", "resolution"), show: nearest }}>
	<Setting name="upscale.resolution" range="1080p, 1440p, 2160p" initial="2160p" />
	<p>
		Sets the short side of the output. A 1920x1080 clip becomes 2560x1440 at 1440p and 3840x2160 at
		2160p.
	</p>
	<p>
		interpolini never scales down. A clip with a short side at or above this value does not change.
	</p>
</Section>

<Section
	id="upscale-method"
	focus={{ rect: middle, panel: "upscale", show: nearest, marks: stairs }}
	control={variants("upscale-method", "Method")}
>
	<Setting name="upscale.method" range="nearest, bilinear, bicubic, lanczos" initial="nearest" />
	<p>
		Sets how the new pixels are drawn. The example enlarges a small part of a 2160p frame, the sword
		edge and the crosshair, so each pixel shows.
	</p>
	<table>
		<tbody>
			<tr>
				<th>nearest</th>
				<td>
					copies the closest pixel. Edges stay hard, and from 1080p to 2160p each pixel becomes a
					2x2 block.
				</td>
			</tr>
			<tr><th>bilinear</th><td>mixes the 4 closest pixels. The result is soft.</td></tr>
			<tr><th>bicubic</th><td>mixes 16 pixels and keeps more sharpness</td></tr>
			<tr>
				<th>lanczos</th>
				<td>the sharpest of the four. It can draw a light halo along strong edges.</td>
			</tr>
		</tbody>
	</table>
	<p>In this example, bicubic and lanczos look almost the same.</p>
</Section>
