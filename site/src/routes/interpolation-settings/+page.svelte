<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import Setting from "#lib/components/Setting.svelte";
	import Term from "#lib/components/Term.svelte";
	import { bladeEdge, middle, swordTip, wallEdge } from "#lib/callouts.ts";
	import { choices, tile, variants, type Control } from "#lib/show.ts";
	import { at, regions, type Spot } from "#lib/window.ts";

	const svp: Spot = { rect: middle, panel: "interpolation" };
	const rife: Spot = { rect: middle, panel: "rife" };
	const engines: Control = {
		kind: "choose",
		label: "Engine",
		initial: "svp",
		options: choices("engine").map((option) =>
			option.label === "rife" ? { ...option, spot: rife } : option
		)
	};
</script>

<Section
	id="interp-section"
	focus={{ ...at("interpolation", "header"), show: tile("engine", "svp") }}
>
	<Setting name="interpolation.enabled" range="yes, no" initial="yes" />
	<p>
		With interpolation off, <Term id="blending">blending</Term> averages the recorded frames, or the frames
		of pre-interpolation when that is on.
	</p>
	<p>
		Except for Frame rate, the previews on this page interpolate a 60 fps copy of the clip to 240
		fps, so that recorded can show the frame of the 240 fps clip at the same moment.
	</p>
</Section>

<Section
	id="interp-engine"
	focus={{ ...svp, show: tile("engine", "svp"), marks: bladeEdge }}
	control={engines}
>
	<Setting name="interpolation.engine" range="svp, rife" initial="svp" />
	<p>
		Sets how interpolini finds the motion between two frames. The svp engine follows the motion of
		small blocks of pixels. The rife engine uses a neural network. Here, svp breaks up the sword
		edge, and rife keeps it closer to the recorded frame, but neither engine wins on every clip.
		Which one to use depends on your footage and on the look you like.
	</p>
	<p>
		Each engine shows its own settings in the panel. The svp engine can also run after
		<Term id="pre-section">pre-interpolation</Term>.
	</p>
</Section>

<Section
	id="interp-fps"
	focus={{ ...svp, show: tile("fps", "960"), marks: wallEdge }}
	control={variants("fps", "Frame rate", "960")}
>
	<Setting name="interpolation.fps" range="2 to 100000, or 1x to 1000x" initial="960" />
	<p>
		Sets the frame rate of the interpolated frames, in fps or as a multiple such as 4x. A multiple
		applies to the clip rate, or to the <Term id="pre-fps">pre-interpolation</Term> rate when that is
		on.
	</p>
	<p>
		<Term id="blending">Blending</Term> takes its frames from this stream, so a higher rate gives a smoother
		blur. With blending off, this is the output frame rate. The preview blends each rate to 60 fps. At
		120, the wall edge shows 3 separate copies. At 960, it is the average of 17 frames.
	</p>
</Section>

<Section
	id="interp-speed"
	focus={{ ...svp, show: tile("speed", "faster"), marks: swordTip }}
	control={variants("speed", "Speed", "faster")}
>
	<Setting name="interpolation.speed" range="medium, fast, faster, fastest" initial="faster" />
	<p>
		Sets how closely svp searches for motion. Smaller blocks can follow smaller shapes, but the
		result depends on the clip. Here, fast keeps the sword best and fastest breaks it apart.
	</p>
	<table>
		<tbody>
			<tr>
				<th>medium</th>
				<td>8 px blocks, or 16 px without a GPU, with refinement</td>
			</tr>
			<tr>
				<th>fast</th>
				<td>16 px blocks, with refinement</td>
			</tr>
			<tr><th>faster</th><td>16 px blocks, no refinement</td></tr>
			<tr><th>fastest</th><td>32 px blocks, no refinement</td></tr>
		</tbody>
	</table>
</Section>

<Section
	id="interp-tuning"
	focus={{ ...svp, show: tile("tuning", "film"), marks: swordTip }}
	control={variants("tuning", "Tuning", "film")}
>
	<Setting name="interpolation.tuning" range="weak, smooth, film, animation" initial="film" />
	<p>Sets what svp expects from the clip.</p>
	<table>
		<tbody>
			<tr><th>weak</th><td>a more thorough search for motion</td></tr>
			<tr>
				<th>smooth, film</th>
				<td>the standard search, with the same result. The example shows film.</td>
			</tr>
			<tr>
				<th>animation</th>
				<td>32 px blocks for flat drawn pictures. Warps edges in a game.</td>
			</tr>
		</tbody>
	</table>
</Section>

<Section
	id="interp-look"
	focus={{ ...svp, show: tile("look", "standard"), marks: swordTip }}
	control={variants("look", "Look", "standard")}
>
	<Setting name="interpolation.algorithm" range="2, 13, 23" initial="13" />
	<p>
		Sets the SVP algorithm for the interpolated frames. The panel names the values sharp (2),
		standard (13) and smooth (23). Here, sharp keeps the sword whole with a blue fringe, and
		standard leaves holes in it. Smooth gives the same frames as standard in this clip. Which look
		is right is a matter of taste.
	</p>
</Section>

<Section
	id="interp-half"
	focus={{ ...svp, show: tile("half", "yes"), marks: swordTip }}
	control={variants("half", "Half-size analysis", "yes")}
>
	<Setting name="interpolation.half size" range="yes, no" initial="yes" />
	<p>
		Makes svp search for motion on a picture of half the size. The result depends on the clip. Here,
		no erases most of the sword. It only works when the width and height of the clip are divisible
		by 4.
	</p>
</Section>

<Section
	id="interp-rife"
	focus={{
		...at("rife"),
		mark: { ...regions.rife.backend, h: regions.rife.backend.h + regions.rife.model.h },
		show: tile("engine", "rife")
	}}
>
	<p>
		The rife engine has the same <Term id="pre-backend">Backend</Term> and
		<Term id="pre-model">Model</Term> settings as pre-interpolation, so a change in one place applies
		to both.
	</p>
</Section>
