<script lang="ts">
	import KeyList from "#lib/components/KeyList.svelte";
	import Section from "#lib/components/Section.svelte";
	import Setting from "#lib/components/Setting.svelte";
	import { chatGhost, chatLines, grass, left, softSeam, wallGrain } from "#lib/callouts.ts";
	import { tile, variants, type Control } from "#lib/show.ts";
	import { at, on, regions, within } from "#lib/window.ts";

	const masked = tile("masking", "image");
	const menus: Control = {
		kind: "choose",
		label: "Menu",
		options: [
			{ label: "none", show: masked },
			{
				label: "add",
				show: masked,
				scene: "mask-add-menu",
				spot: on("mask-add-menu", "menu-gallery", "menu")
			},
			{
				label: "tile",
				show: masked,
				scene: "mask-menu",
				spot: on("mask-menu", "menu-gallery", "menu")
			}
		]
	};
	const editor = { ...on("mask-editor", "masking"), panel: "image" } as const;
	const painted = {
		scene: "mask-editor",
		panel: "image",
		rect: within(regions.preview, { x: 0, y: 0.5, w: 0.5, h: 0.5 })
	} as const;
	const keys = [
		["Erase", "Right drag", "Alt drag"],
		["Paint or erase", "X", "E"],
		["Brush, box, ellipse", "B", "R", "O"],
		["Brush size", "[", "]"],
		["Straight line", "Shift click"],
		["Square or circle", "Shift drag"],
		["Invert", "Ctrl I"],
		["Zoom, move the view", "Wheel", "Middle drag"],
		["Cancel", "Esc"]
	] as const;
</script>

<Section id="masking-section" focus={{ ...at("peel", "method"), show: tile("masking", "peel") }}>
	<Setting name="mask.enabled" range="yes, no" initial="yes" />
	<p>Masking keeps fixed parts of the picture, such as the HUD, from warping.</p>
	<p>
		The examples on this page use a 60 fps copy of the clip, where the errors are larger. Most show
		one interpolated frame without blending.
	</p>
</Section>

<Section
	id="mask-method"
	focus={{ rect: left, panel: "peel", show: tile("masking", "off"), marks: chatLines }}
	control={variants("masking", "Masking", "off")}
>
	<Setting name="mask.method" range="peel, still, image" initial="peel" />
	<table>
		<tbody>
			<tr>
				<th>peel</th>
				<td>
					removes the HUD before interpolation and restores it after, also when the HUD is
					transparent
				</td>
			</tr>
			<tr>
				<th>still</th>
				<td>fades pixels that do not change between frames instead of warping them</td>
			</tr>
			<tr>
				<th>image</th>
				<td>keeps the original frame in the parts that a mask picture marks</td>
			</tr>
		</tbody>
	</table>
	<p>
		With masking off, the chat smears when the camera turns. Peel keeps it sharp. Still leaves faint
		copies of some letters. Image keeps the original frame in a box over the chat, so the wall in
		the box does not move with the scene.
	</p>
</Section>

<Section
	id="mask-peel"
	focus={{ rect: left, panel: "peel", show: tile("peel-limit", "20"), marks: chatGhost }}
	control={variants("peel-limit", "Peel limit", "20")}
>
	<Setting name="mask.peel limit" range="1 to 20" initial="6" />
	<p>
		Peel counts a pixel as HUD when it does not follow the motion of the scene. A higher limit marks
		fewer pixels as HUD.
	</p>
	<p>
		At 20, parts of the chat do not count as HUD and smear. Values below 6 give the same result in
		this clip.
	</p>
</Section>

<Section
	id="mask-still"
	focus={{ rect: left, panel: "still", show: tile("still-limit", "20"), marks: grass }}
	control={variants("still-limit", "Still limit", "20")}
>
	<Setting name="mask.limit" range="0 to 20" initial="4.5" />
	<p>
		A pixel counts as still when it changes by no more than this value between two frames, on a
		scale of 0 to 255. Raising it marks more pixels as still.
	</p>
	<p>This example blends. At 20, the moving grass counts as still and gets no motion blur.</p>
</Section>

<Section
	id="mask-edge"
	focus={{ rect: left, panel: "still", show: tile("still-edge", "0"), marks: wallGrain }}
	control={variants("still-edge", "Edge", "0")}
>
	<Setting name="mask.edge" range="0 to 20" initial="5" />
	<p>
		Edge and Tolerance belong to the still method. A pixel also counts as still when its detail is
		stronger than Edge, on a scale of 0 to 255, and stays the same between two frames. Lowering it
		lets weaker detail count.
	</p>
	<p>
		At 0, the texture of the moving wall counts as still and shows as grain. Tolerance is 100 in
		this comparison.
	</p>
</Section>

<Section
	id="mask-tolerance"
	focus={{ rect: left, panel: "still", show: tile("still-tolerance", "100"), marks: wallGrain }}
	control={variants("still-tolerance", "Tolerance", "100")}
>
	<Setting name="mask.tolerance" range="0 to 100%" initial="30%" />
	<p>
		Sets how much that detail may change between two frames, as a percent of its strength. Raising
		it marks more pixels as still.
	</p>
	<p>
		At 100, the texture of the moving wall counts as still and shows as grain. Edge is 0 in this
		comparison.
	</p>
</Section>

<Section id="mask-gallery" focus={{ ...at("image", "gallery"), show: masked }} control={menus}>
	<Setting name="mask.image" range="a mask name" initial="none" />
	<p>
		A mask is a PNG file in the <code>masks</code> folder next to the app, and black in it keeps the original
		frame. Click a mask to use it, or hover over one to see it on the preview.
	</p>
	<p>
		To add a mask, press + to draw one on the clip or import an image, or drop PNG, JPG, WebP or BMP
		files on the section. Right-click a mask for more actions.
	</p>
</Section>

<Section
	id="mask-soft"
	focus={{ rect: left, panel: "image", show: tile("soft-edge", "0"), marks: softSeam }}
	control={variants("soft-edge", "Soft edge", "0")}
>
	<Setting name="mask.soft edge" range="0 to 100" initial="0" />
	<p>
		Appears once you choose a mask, and in the editor. Blurs the mask out from its edge, so the
		original frame fades into the interpolated frame. The value is in pixels at 1080p.
	</p>
	<p>
		At 0, the wall in the box ends at the top edge of the box. At 100, it fades out above the box.
	</p>
</Section>

<Section id="mask-editor" focus={editor}>
	<p>
		To open the editor in the Masking section, choose Draw on the clip or Edit, or double-click a
		mask. Paint the parts of the preview that keep the original frame. Save writes the mask to the
		<code>masks</code> folder under Name.
	</p>
</Section>

<Section id="mask-keys" focus={painted}>
	<KeyList rows={keys} />
</Section>
