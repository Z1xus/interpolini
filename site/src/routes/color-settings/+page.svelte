<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import Setting from "#lib/components/Setting.svelte";
	import Term from "#lib/components/Term.svelte";
	import { tile, variants } from "#lib/show.ts";
	import { at } from "#lib/window.ts";
	import { middle } from "#lib/callouts.ts";
</script>

<Section id="color-section" focus={{ ...at("color"), show: tile("color-lut", "look") }}>
	<Setting name="color.enabled" range="yes, no" initial="no" />
	<p>
		interpolini changes the colors after <Term id="blending">blending</Term> and before
		<Term id="upscale-section">upscaling</Term>.
	</p>
</Section>

<Section
	id="color-brightness"
	focus={{
		rect: middle,
		panel: "color",
		show: tile("color-brightness", "1")
	}}
	control={variants("color-brightness", "Brightness", "1")}
>
	<Setting name="color.brightness" range="0 to 2" initial="1" />
	<p>
		Multiplies the brightness of each pixel. 1 changes nothing. A bright area clips to white and
		loses its detail.
	</p>
</Section>

<Section
	id="color-contrast"
	focus={{
		rect: middle,
		panel: "color",
		show: tile("color-contrast", "1")
	}}
	control={variants("color-contrast", "Contrast", "1")}
>
	<Setting name="color.contrast" range="0 to 2" initial="1" />
	<p>
		Moves the brightness of each pixel away from mid gray. 1 changes nothing. A lower value flattens
		the picture.
	</p>
</Section>

<Section
	id="color-saturation"
	focus={{
		rect: middle,
		panel: "color",
		show: tile("color-saturation", "1")
	}}
	control={variants("color-saturation", "Saturation", "1")}
>
	<Setting name="color.saturation" range="0 to 2" initial="1" />
	<p>Scales the strength of the colors. 1 changes nothing, and 0 makes the picture gray.</p>
</Section>

<Section
	id="color-hue"
	focus={{
		rect: middle,
		panel: "color",
		show: tile("color-hue", "0")
	}}
	control={variants("color-hue", "Hue", "0")}
>
	<Setting name="color.hue" range="-180 to 180 degrees" initial="0 degrees" />
	<p>
		Turns every color around the color wheel. 0 changes nothing, and 180 gives the opposite colors.
	</p>
</Section>

<Section
	id="color-lut"
	focus={{
		rect: middle,
		panel: "color",
		show: tile("color-lut", "none")
	}}
	control={variants("color-lut", "LUT", "none")}
>
	<Setting name="color.lut" range="a .cube file" initial="none" />
	<p>
		Applies a 3D LUT from a <code>.cube</code> file after the four settings above. Choose picks a file,
		and × removes it. The example LUT tints shadows blue and highlights warm.
	</p>
</Section>
