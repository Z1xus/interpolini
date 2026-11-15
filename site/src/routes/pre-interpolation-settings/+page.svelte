<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import Setting from "#lib/components/Setting.svelte";
	import Term from "#lib/components/Term.svelte";
	import { middle, swordTip } from "#lib/callouts.ts";
	import { choices, differences, tile, variants, type Control } from "#lib/show.ts";
	import { beside, regions, type Spot } from "#lib/window.ts";

	const pre = variants("pre", "Pre-interpolation", "off");
	const rates: Control = {
		kind: "choose",
		label: "Frame rate",
		initial: "2x",
		options: choices("pre").filter(({ label }) => label !== "off")
	};
	const backends = differences("backend", "Backend", "tensorrt");
	const models = variants("models", "Model");
	const picture: Spot = { rect: middle, panel: "pre" };
</script>

<Section
	id="pre-section"
	focus={{ panel: "pre", rect: regions.window, show: tile("pre", "off"), marks: swordTip }}
	control={pre}
>
	<Setting name="pre-interpolation.enabled" range="yes, no" initial="no" />
	<p>
		RIFE raises the frame rate first, and the svp engine interpolates the result from closer frames.
		The render is slower.
	</p>
	<p>
		The previews interpolate a 60 fps copy of the clip to 240 fps. Pick recorded to compare with the
		real 240 fps frame. Here, svp alone loses the sword, and pre-interpolation keeps it.
	</p>
</Section>

<Section
	id="pre-fps"
	focus={{ ...picture, show: tile("pre", "2x"), marks: swordTip }}
	control={rates}
>
	<Setting name="pre-interpolation.fps" range="2 to 100000, or 1x to 1000x" initial="2x" />
	<p>
		Sets the frame rate that RIFE makes. Enter a rate in fps, or a multiple of the clip rate such as
		2x or 2.5x.
	</p>
	<p>
		A higher rate gives svp closer frames, but the render is slower. When it matches the <Term
			id="interp-fps">interpolation frame rate</Term
		>, RIFE makes all the frames.
	</p>
</Section>

<Section
	id="pre-backend"
	focus={{ ...beside("pre", "backend"), show: tile("backend", "vulkan") }}
	control={backends}
>
	<Setting name="rife.backend" range="vulkan, tensorrt" initial="vulkan" />
	<p>
		Sets how RIFE runs on the GPU. The vulkan backend works on any GPU with Vulkan. The tensorrt
		backend needs an NVIDIA GPU and the <Term id="components-tensorrt">TensorRT component</Term>.
	</p>
	<p>The frames of the two backends are almost identical, as the difference shows.</p>
</Section>

<Section id="pre-model" focus={{ ...picture, show: tile("models", "rife-v4.6") }} control={models}>
	<Setting
		name="rife.model"
		range="rife-v4.6, rife-v4.15-lite, rife-v4.22-lite, rife-v4.25, rife-v4.25-lite, rife-v4.26"
		initial="rife-v4.6"
	/>
	<p>
		Sets the RIFE model. The list shows the installed models, and Get more models opens
		<Term id="components-rife">Components</Term>.
	</p>
	<p>
		These examples run RIFE alone at 4x. Each model breaks the sword edge in its own way, and here
		rife-v4.15-lite blurs the right edge. No model is best for every clip, so compare them on yours.
	</p>
</Section>
