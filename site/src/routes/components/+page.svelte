<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import Term from "#lib/components/Term.svelte";
	import { on, stages, within } from "#lib/window.ts";

	const rect = within(stages.components.dialog, { x: 0, y: 0, w: 1, h: 0.8 });
</script>

<Section id="components-open" focus={{ ...on("components", "dialog", "tab"), rect }}>
	<p>Choose File &gt; Settings and open Components.</p>
</Section>

<Section id="components-rife" focus={{ ...on("components", "dialog", "rife"), rect }}>
	<p>Each RIFE model is a separate download. Removing a model also deletes its TensorRT engine.</p>
</Section>

<Section
	id="components-tensorrt"
	focus={{ ...on("components-install", "dialog", "tensorrt"), rect }}
>
	<p>
		TensorRT needs an NVIDIA GPU with driver 525.60 or newer on Linux, or 527.41 or newer on
		Windows. Install (3.1 GB) downloads the NVIDIA libraries.
	</p>
	<p>
		Set Backend to tensorrt in the Interpolation section. The first run with each model takes
		longer, because interpolini builds a TensorRT engine for it.
	</p>
</Section>

<Section id="components-ffmpeg" focus={{ ...on("components", "dialog", "ffmpeg"), rect }}>
	<p>
		Source sets which FFmpeg libraries interpolini uses: the bundled ones that come with the app,
		the system ones on Linux, or custom ones from a folder that you pick with Choose folder.
	</p>
	<p>
		The libraries must have the same version as the bundled ones. Restart applies the change. <Term
			id="elsewhere-ffmpeg">More about FFmpeg</Term
		>.
	</p>
</Section>
