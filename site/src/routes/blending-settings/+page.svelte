<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import Setting from "#lib/components/Setting.svelte";
	import Term from "#lib/components/Term.svelte";
	import { wallMotion } from "#lib/callouts.ts";
	import { weightings } from "#lib/weights.ts";
	import { blended, type Control } from "#lib/show.ts";
	import { at, regions, type Spot } from "#lib/window.ts";

	const picture: Spot = { rect: regions.preview, panel: "blending" };
	const range = (
		label: string,
		field: "fps" | "intensity" | "gamma",
		min: number,
		max: number,
		step: number
	): Control => ({ kind: "range", label, field, min, max, step });

	const fps = range("Frame rate", "fps", 30, 240, 1);
	const intensity = range("Intensity", "intensity", 0, 4, 0.1);
	const gamma = range("Gamma", "gamma", 1, 10, 0.1);
	const weighting: Control = {
		kind: "choose",
		label: "Weighting",
		options: weightings.map((name) => ({
			label: name,
			show: blended({ weighting: name })
		}))
	};
</script>

<Section id="blending-section" focus={{ ...at("blending", "curve"), show: blended() }}>
	<Setting name="blending.enabled" range="yes, no" initial="yes" />
	<p>The graph shows how many frames blend and the weight of each frame.</p>
	<p>
		Blending runs only when Frame rate is below the rate of the frames that reach it. These come
		from interpolation, or from pre-interpolation or the clip when interpolation is off.
	</p>
</Section>

<Section id="blending-fps" focus={{ ...picture, show: blended(), marks: wallMotion }} control={fps}>
	<Setting name="blending.fps" range="1 to 1000" initial="60" />
	<p>
		Sets the output frame rate. A lower rate blends more <Term id="interpolation">interpolated</Term
		> frames into each output frame, so the blur is longer. From 960 fps, a blend has 5 frames at 240
		fps, 17 at 60 fps and 33 at 30 fps.
	</p>
</Section>

<Section
	id="blending-intensity"
	focus={{ ...picture, show: blended(), marks: wallMotion }}
	control={intensity}
>
	<Setting name="blending.intensity" range="0 to 4" initial="1" />
	<p>
		Scales how many frames blend into each output frame. At 1 the blend covers the time of one
		output frame. At 2 the blur is twice as long, and at 0 there is no blur. How much blur looks
		good is up to you.
	</p>
</Section>

<Section
	id="blending-gamma"
	focus={{ ...picture, show: blended({ gamma: 10 }), marks: wallMotion }}
	control={gamma}
>
	<Setting name="blending.gamma" range="1 to 10" initial="1" />
	<p>
		Sets how brightness blends. At 1 the blend is a plain average, and a bright spot fades into the
		dark around it. Raising it gives bright pixels more weight, so the spot leaves a bright streak.
		Only brightness blends this way. Colors blend as a plain average.
	</p>
</Section>

<Section
	id="blending-weighting"
	focus={{ ...picture, show: blended(), marks: wallMotion }}
	control={weighting}
>
	<Setting name="blending.weighting" range={weightings.join(", ")} initial="equal" />
	<p>
		Sets how much each blended frame counts. The graph in the <Term id="blending-section"
			>Blending section</Term
		> shows the weights from the first frame to the last. No weighting is correct, so pick the blur you
		like.
	</p>
	<table>
		<tbody>
			<tr><th>equal</th><td>every frame counts the same</td></tr>
			<tr><th>pyramid</th><td>the middle frame counts most</td></tr>
			<tr><th>gaussian_sym</th><td>the middle frame counts most, in a bell curve</td></tr>
			<tr><th>ascending</th><td>later frames count more</td></tr>
			<tr><th>gaussian</th><td>later frames count more, in a bell curve</td></tr>
			<tr><th>descending</th><td>earlier frames count more</td></tr>
			<tr>
				<th>vegas</th>
				<td>equal, but the two end frames count half when the count is even</td>
			</tr>
			<tr><th>custom</th><td>a list of weights, such as [1, 2, 5]</td></tr>
		</tbody>
	</table>
</Section>
