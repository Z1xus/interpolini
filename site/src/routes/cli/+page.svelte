<script lang="ts">
	import CodeBlock from "#lib/components/CodeBlock.svelte";
	import Section from "#lib/components/Section.svelte";
	import Term from "#lib/components/Term.svelte";
	import { at, on, regions } from "#lib/window.ts";

	const options = [
		["-c, --config <name|file>", "Load a named config or an ini file"],
		["-s, --set <setting>", "Change one setting"],
		["--from <seconds>", "Start of the part to render"],
		["--to <seconds>", "End of the part to render"],
		["--mute <track>", "Leave out an audio track"],
		["-1, --join", "Put all clips into one file"],
		["-g, --global", "Skip the configs in the folders of the clips"],
		["-j, --json", "Print events as JSON lines"],
		["-h, --help", "Show the usage"],
		["-V, --version", "Show the version"]
	] as const;

	const usage = `interpolini-cli clip.mp4
interpolini-cli first.mp4 second.mp4`;

	const settings = `interpolini-cli -s blending.fps=120 -s output.codec=hevc clip.mp4
interpolini-cli -s "mask.peel limit=10" -s mask.enabled=no clip.mp4`;

	const range = `interpolini-cli --from 10 --to 25 clip.mp4
interpolini-cli --mute 2 clip.mp4
interpolini-cli --join first.mp4 second.mp4`;

	const batch = `interpolini-cli -c fast *.mp4
interpolini-cli --global *.mp4
interpolini-cli --json clip.mp4`;

	const events = `{"event":"start","clip":"clip.mp4"}
{"event":"progress","frame":120,"frames":1800,"fps":96.5}
{"event":"done","clip":"clip.mp4","output":"clip - interpolini.mp4","seconds":18.7}`;

	const ini = `; a line that starts with ; or # is a comment
[interpolation]
engine: rife
fps: 480

[rife]
backend: tensorrt
model: rife-v4.25

[blending]
fps: 60
weighting: equal

[output]
codec: hevc
quality: 18`;
</script>

<Section id="cli-usage" focus={{ rect: regions.window, mark: regions.render }}>
	<p>
		<code>interpolini-cli</code> renders clips without the window. It is in the same folder as the
		app. It saves each result where <Term id="render">Render</Term> does.
	</p>
	<CodeBlock code={usage} lang="shell" />
	<dl class="my-4 divide-y divide-seam border-y border-seam text-sm">
		{#each options as [option, description] (option)}
			<div class="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 py-1.5">
				<dt><code>{option}</code></dt>
				<dd class="text-muted">{description}</dd>
			</div>
		{/each}
	</dl>
	<p>
		The CLI reads the same <Term id="config-file">config files</Term> and the same settings as the window.
	</p>
</Section>

<Section id="cli-settings" focus={at("blending", "fps")}>
	<p>
		To change one setting for this run, pass <code class="whitespace-nowrap"
			>-s section.key=value</code
		>. Repeat it for more settings. The names are the same as in the config file, and a key with a
		space needs quotes.
	</p>
	<CodeBlock code={settings} lang="shell" />
</Section>

<Section id="cli-range" focus={on("plain", "editor-joint", "cut-tools")}>
	<p>
		To render a part of the clip, pass <code>--from</code> and <code>--to</code> in seconds. To
		leave out an audio track, pass <code>--mute</code> with its number, for example
		<code class="whitespace-nowrap">--mute 2</code>
		for the second track. To put all clips into one file in the order you give them, pass
		<code>--join</code>.
	</p>
	<CodeBlock code={range} lang="shell" />
</Section>

<Section id="cli-batch" focus={on("plain", "panel-top", "config")}>
	<p>interpolini renders the clips one after the other, and each clip takes its own config.</p>
	<CodeBlock code={batch} lang="shell" />
	<p>
		With <code>--json</code>, the CLI prints one JSON line for each event: start, info, warning,
		preparing, progress, done and failed.
	</p>
	<CodeBlock code={events} lang="json" />
	<p>
		The exit code is 0 when all clips are done, 1 when a clip failed, 2 for wrong arguments and 130
		when you stop the render.
	</p>
</Section>

<Section id="cli-wrap" focus={{ rect: regions.window }}>
	<p>
		To use <code>interpolini-cli</code> from a script or another app, run it with
		<code class="whitespace-nowrap">--json</code>
		and read the events from stdout. Take the path of the result from the <code>done</code> event, because
		interpolini never overwrites a file and adds a number to the name when it is taken.
	</p>
	<p>
		Pass the arguments as a list and not as one shell string, and give each clip as a full path.
		Always pass at least one clip, because without one the CLI opens a
		<Term id="cli-picker">file dialog</Term> and waits. Pass <code>-c</code> with the path of your
		own ini file as well, so the settings don't depend on an <code>interpolini.ini</code> next to the
		clip or the app.
	</p>
	<p>
		If what you're running is public, please don't pass arbitrary or user-controlled strings in
		<code>-s</code> or the config:
		<Term id="output-options">output.options</Term> goes to the encoder as it is, and
		<code>color.lut</code> makes interpolini read a file. Clips from other people are a risk of their
		own, because FFmpeg decodes them inside the process. For those, run the CLI in a sandbox or a container
		with a time limit and without network access, which it never needs.
	</p>
	<p>
		To stop a render, send one interrupt, which is Ctrl C, or SIGTERM on Linux and macOS.
		interpolini closes the file at the next frame, so it is shorter but still plays, and exits with
		code 130. A second interrupt quits at once and deletes the unfinished file. Don't kill the
		process, because that leaves a broken file behind.
	</p>
</Section>

<Section id="cli-picker" focus={on("plain", "editor-joint", "import")}>
	<p>
		Start <code>interpolini-cli</code> without a clip to choose clips in a file dialog.
	</p>
</Section>

<Section id="config-file" focus={on("plain", "panel-top", "config")}>
	<p>
		interpolini reads its settings from a text file named <code>interpolini.ini</code>. A setting
		that is missing from the file uses its default.
	</p>
	<CodeBlock code={ini} lang="ini" />
	<p>
		To save the settings of the panel to this file, press Save in the Config row. New creates a
		named config.
	</p>
</Section>

<Section id="config-lookup" focus={on("plain", "panel-top", "config")}>
	<p>For each clip, interpolini uses the first config it finds:</p>
	<ol>
		<li><code>interpolini.ini</code> in the folder of the clip</li>
		<li><code>interpolini.ini</code> in the folder of the app</li>
		<li>the built-in defaults</li>
	</ol>
	<p>
		With <code class="whitespace-nowrap">--config name</code>, it looks for
		<code>interpolini.name.ini</code> instead.
	</p>
</Section>
