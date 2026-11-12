<script lang="ts">
	import { moment } from "#lib/assets.ts";
	import { chatLines, left, wallMotion } from "#lib/callouts.ts";
	import Section from "#lib/components/Section.svelte";
	import Term from "#lib/components/Term.svelte";
	import { blended, tile, variants, type Control } from "#lib/show.ts";
	import { regions } from "#lib/window.ts";

	const frames: Control = {
		kind: "frames",
		label: "Frame",
		from: 0,
		to: moment.frames - 1
	};
	const blending: Control = {
		kind: "choose",
		label: "Blending",
		initial: "On",
		options: [
			{ label: "Off", show: blended({ intensity: 0 }) },
			{ label: "On", show: blended() }
		]
	};
</script>

<Section
	id="interpolation"
	focus={{
		rect: regions.preview,
		show: blended({ intensity: 0 }),
		tag: (frame) => (frame % moment.real === 0 ? "Recorded frame" : "Interpolated frame")
	}}
	control={frames}
>
	<p>
		Your video is a series of frames, usually 60 or more per second. Interpolation estimates the
		motion between two frames, which makes it possible to add more frames in between.
	</p>
	<p>
		For example, this clip was recorded at 240 fps. At 960 fps, three interpolated frames come
		between each pair of recorded frames.
	</p>
	<p>
		The more frames you record, the less the picture changes from one frame to the next, and the
		more accurate the frames in between are.
	</p>
</Section>

<Section
	id="blending"
	focus={{
		rect: regions.preview,
		panel: "blending",
		show: blended({ intensity: 0 }),
		marks: wallMotion
	}}
	control={blending}
>
	<p>
		With that many frames, you can blend them. Blending averages nearby frames into one, which gives
		the <a href="https://en.wikipedia.org/wiki/Motion_blur_(media)" target="_blank" rel="noreferrer"
			>motion blur</a
		> that a camera makes with a long shutter.
	</p>
	<p>
		At the defaults, each 60 fps output frame is the average of 17 interpolated frames, and
		<Term id="blending-intensity">Intensity</Term> sets how many frames blend.
	</p>
</Section>

<Section
	id="artifacts"
	focus={{ rect: regions.preview, show: tile("artifacts", "svp") }}
	control={variants("artifacts", "Frame", "svp")}
>
	<p>
		The estimate is not always right. When an object moves fast or changes shape between two frames,
		interpolation can tear it apart or leave holes in it. These errors are called artifacts.
	</p>
	<p>
		Here, a 60 fps copy of a clip was interpolated to 240 fps, so you can compare the result with
		the frame that was recorded at the same moment. SVP tears the sword apart, and RIFE leaves small
		holes in it.
	</p>
</Section>

<Section
	id="fighting-artifacts"
	focus={{ rect: left, show: tile("masking", "off"), marks: chatLines }}
	control={variants("masking", "Masking", "off")}
>
	<p>
		The first thing to try is recording at a higher frame rate, since more recorded frames make the
		frames in between more accurate. The <Term id="interp-engine">rife</Term> engine is usually more accurate
		than svp too, but it takes longer to render. With svp, <Term id="interp-speed" /> and
		<Term id="pre-section">pre-interpolation</Term> also change how many artifacts you see.
	</p>
	<p>
		interpolini also has three <Term id="masking-section">masking</Term> methods that protect fixed parts
		of the picture, such as the HUD, from these artifacts: <Term id="mask-peel">peel</Term>,
		<Term id="mask-still">still</Term> and <Term id="mask-gallery">image</Term>.
	</p>
</Section>
