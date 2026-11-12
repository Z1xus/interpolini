<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import Term from "#lib/components/Term.svelte";
	import { on } from "#lib/window.ts";
</script>

<Section id="scope-all" focus={on("selected", "panel-top", "scope")}>
	<p>
		Settings for decides which clips your changes reach. By default that is all clips, so a change
		in the panel goes to every clip on the timeline.
	</p>
</Section>

<Section id="scope-clip" focus={on("scope-clip", "panel-top", "scope")}>
	<p>
		To give a clip its own settings, select it and set Settings for to this clip. Your changes then
		go only to that clip, or to every selected clip when you select several.
	</p>
</Section>

<Section id="scope-config" focus={on("plain", "panel-top", "config")}>
	<p>
		A new clip takes its settings from the <code>interpolini.ini</code> in its folder. When the
		folder has no such file, it takes the settings on screen, which start from Default preset in
		Settings &gt; General. <Term id="config-lookup">Lookup order</Term> has the full rules.
	</p>
	<p>To load another config for the selected clip, choose it in the Config list.</p>
</Section>

<Section id="scope-share" focus={on("share", "share-area", "popup")}>
	<p>
		To share your config, open Share and press Copy, which puts the settings of the panel on the
		clipboard as a short code. A LUT file is not part of the code.
	</p>
	<p>
		To use a code from someone else, paste it under Use a code and press Apply. Like any other
		change, it goes to the clips that <Term id="scope-all">Settings for</Term> is set to.
	</p>
</Section>
