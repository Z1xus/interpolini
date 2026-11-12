<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import Term from "#lib/components/Term.svelte";
	import { tile } from "#lib/show.ts";
	import { at, regions } from "#lib/window.ts";
</script>

<Section
	id="add-clips"
	focus={{ rect: regions.timeline, mark: regions.timeline, show: tile("overview", "original") }}
>
	<p>
		Drop clips into the window. Each clip appears on the timeline. To record clips, see
		<Term id="elsewhere-recording" />.
	</p>
</Section>

<Section
	id="check-settings"
	focus={{ ...at("interpolation", "fps"), show: tile("overview", "result") }}
>
	<p>
		The defaults <Term id="interp-section">interpolate</Term> to 960 fps and
		<Term id="blending-section">blend</Term> to 60 fps. To change a setting, open its section in the panel.
		Each section has its own page under Settings.
	</p>
</Section>

<Section
	id="render"
	focus={{ rect: regions.render, mark: regions.render, show: tile("overview", "result") }}
>
	<p>
		Press <Term id="render-button">Render</Term>. interpolini saves the result next to the source
		clip as <code>name - interpolini.mp4</code>.
	</p>
</Section>
