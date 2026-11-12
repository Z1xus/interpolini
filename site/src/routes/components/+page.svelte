<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import Term from "#lib/components/Term.svelte";
	import { on, stages, within } from "#lib/window.ts";

	const rect = within(stages.components.dialog, { x: 0, y: 0, w: 1, h: 0.8 });
</script>

<Section id="components-open" focus={{ ...on("components", "dialog", "tab"), rect }}>
	<p>
		Choose File &gt; Settings and open Components. The Model list of the Interpolation section ends
		with Get more models, which opens it too.
	</p>
</Section>

<Section id="components-rife" focus={{ ...on("components", "dialog", "rife"), rect }}>
	<p>
		Download fetches a RIFE model and checks the file. Remove deletes the model and its TensorRT
		engine. Cancel stops a download.
	</p>
</Section>

<Section
	id="components-tensorrt"
	focus={{ ...on("components-install", "dialog", "tensorrt"), rect }}
>
	<p>
		TensorRT needs an NVIDIA GPU with driver 525.60 or newer on Linux, or 527.41 or newer on
		Windows. Install (3.1 GB) downloads the NVIDIA libraries, and a model when none is installed.
	</p>
	<p>
		Set Backend to tensorrt in the Interpolation section. The first run with a model shows Preparing
		TensorRT for first use while interpolini builds an engine for it. Remove deletes the libraries.
		Each engine stays until you remove its model.
	</p>
</Section>

<Section id="components-ffmpeg" focus={{ ...on("components", "dialog", "ffmpeg"), rect }}>
	<p>
		Source sets which FFmpeg libraries interpolini uses. The bundled libraries come with the app.
		The system libraries are only an option on Linux. Choose folder takes the libraries of another
		FFmpeg build from a folder, and Source then shows custom.
	</p>
	<p>
		The libraries must have the same version as the bundled ones. Restart applies the change. <Term
			id="elsewhere-ffmpeg">More about FFmpeg</Term
		>.
	</p>
</Section>
