<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import Setting from "#lib/components/Setting.svelte";
	import { middle, wallCopy } from "#lib/callouts.ts";
	import { tile, variants } from "#lib/show.ts";
	import { regions } from "#lib/window.ts";
</script>

<Section
	id="dedup-section"
	focus={{ panel: "dedup", rect: regions.window, show: tile("repeats", "off"), marks: wallCopy }}
	control={variants("repeats", "Dedup", "off")}
>
	<Setting name="dedup.enabled" range="yes, no" initial="yes" />
	<p>
		When a game draws fewer frames than the recording rate, the recording repeats frames, resulting
		in a picture that stops and then jumps. Dedup replaces each repeated frame with an interpolated
		one.
	</p>
	<p>
		In this example the game draws 60 fps in a 240 fps recording, so 3 of every 4 frames are
		repeats. With dedup off the blur shows a separate copy of the wall edge, and with dedup on it is
		smooth.
	</p>
	<p>The header shows the share of repeats when it is 5% or more.</p>
</Section>

<Section
	id="dedup-repeats"
	focus={{ rect: middle, panel: "dedup", show: tile("repeats-replaced", "0"), marks: wallCopy }}
	control={variants("repeats-replaced", "Repeats replaced", "0")}
>
	<Setting name="dedup.repeats" range="0 to 100%" initial="100%" />
	<p>
		Sets how many of the repeated frames dedup replaces, starting with the ones that differ least
		from the frame before. At 50%, the copy of the wall edge is fainter.
	</p>
</Section>
