<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import type { Control } from "#lib/show.ts";
	import { on, pin, regions, within } from "#lib/window.ts";

	const views: Control = {
		kind: "choose",
		label: "View",
		initial: "result",
		options: [
			{ label: "original", scene: "view-original" },
			{ label: "result", scene: "plain" },
			{ label: "split", scene: "view-split" },
			{ label: "side", scene: "view-side" }
		]
	};
</script>

<Section id="preview-views" focus={on("plain", "stage", "tabs")} control={views}>
	<p>
		The tabs above the preview set the view. Original shows the clip as it is. Result shows the clip
		with your settings. Split divides the picture at a line that you drag. Side shows the two next
		to each other.
	</p>
</Section>

<Section
	id="preview-hold"
	focus={{
		...on("held", "stage"),
		marks: [
			{ kind: "label", at: pin(80, 545), text: "hold `" },
			{ kind: "arrow", from: pin(75, 552), to: pin(45, 568) }
		]
	}}
>
	<p>Hold the backtick key (`) to see the original, and release it to go back to your view.</p>
</Section>

<Section
	id="preview-zoom"
	focus={{
		...on("zoomed", "stage", "zoom"),
		rect: within(regions.preview, { x: 0.65, y: 0, w: 0.35, h: 0.3 }),
		marks: [{ kind: "arrow", from: pin(850, 90), to: pin(906, 52) }]
	}}
>
	<p>
		Scroll over the preview to zoom. Drag to move the picture. The zoom button shows Fit or the zoom
		level. Press it to fit the picture, or double-click the picture to return to the default zoom.
	</p>
	<p>
		Default preview zoom in Settings sets that zoom. When it is Fit, a double-click fits the picture
		too.
	</p>
</Section>

<Section id="preview-playback" focus={on("plain", "stage-transport", "resolution")}>
	<p>
		Playback resolution sets the size of the preview: Auto, Full, 1/2 or 1/4. A lower size plays
		more smoothly. Turn off Preview in the right-click menu of the picture to hide the preview.
	</p>
	<p>Preview memory in Settings sets how much memory the preview can use.</p>
</Section>

<Section
	id="preview-frame"
	focus={{
		...on("plain", "transport", "save-frame"),
		marks: [{ kind: "label", at: pin(640, 597), text: "Ctrl Shift E" }]
	}}
>
	<p>
		Save frame, or Ctrl Shift E, saves the picture in the preview as a PNG file next to the source
		clip. A note appears with Copy file and Show in folder.
	</p>
</Section>

<Section id="preview-transform" focus={on("transform-move", "stage-transport", "transform")}>
	<p>
		Press Transform to show the transform box of the selected clip. The box moves, scales and crops
		the clip on the canvas.
	</p>
	<p>Double-click the box to reset it.</p>
</Section>

<Section
	id="preview-move"
	focus={{
		...on("transform-move", "stage", "box"),
		marks: [
			{ kind: "arrow", from: pin(560, 260), to: pin(470, 330) },
			{ kind: "label", at: pin(600, 300), text: "drag" }
		]
	}}
>
	<p>Drag the box to move the clip. The arrow keys move it by 1 pixel, and by 10 with Shift.</p>
</Section>

<Section
	id="preview-scale"
	focus={{
		...on("transform-scale", "stage", "corner"),
		marks: [
			{ kind: "arrow", from: pin(196, 176), to: pin(262, 226) },
			{ kind: "label", at: pin(272, 262), text: "drag" }
		]
	}}
>
	<p>
		Drag a corner to scale the clip around its center. The plus and minus keys scale it by 5
		percent.
	</p>
</Section>

<Section
	id="preview-crop"
	focus={{
		...on("transform-crop", "stage", "edge-left"),
		marks: [
			{ kind: "arrow", from: pin(190, 326), to: pin(262, 326) },
			{ kind: "label", at: pin(205, 300), text: "drag in" }
		]
	}}
>
	<p>Drag the handle on an edge to crop that side.</p>
</Section>

<Section
	id="preview-stretch"
	focus={{
		...on("transform-stretch", "stage", "edge-right"),
		marks: [
			{ kind: "arrow", from: pin(748, 326), to: pin(826, 326) },
			{ kind: "label", at: pin(750, 300), text: "Shift + drag" }
		]
	}}
>
	<p>Shift drag the handle on an edge to stretch the clip on that axis instead of cropping it.</p>
</Section>
