<script lang="ts">
	import KeyList from "#lib/components/KeyList.svelte";
	import Section from "#lib/components/Section.svelte";
	import Term from "#lib/components/Term.svelte";
	import { on, pin } from "#lib/window.ts";

	const keys = [
		["Play or pause", "Space", "K"],
		["Step 1 frame", "Left", "Right"],
		["Back or forward 1 second", "J", "L", "Shift Left", "Shift Right"],
		["Step to a keyframe", "Alt Left", "Alt Right"],
		["Go to start or end", "Home", "End"],
		["Volume", "Up", "Down"],
		["Mute", "M"],
		["Next or previous clip", "]", "["],
		["Set start or end", "I", "O"],
		["Split", "S", "Ctrl K", "Ctrl B"],
		["Remove clip", "Delete", "Backspace"],
		["Ripple delete", "Shift Delete"],
		["Restore full length", "X"],
		["Close the gap", "G"],
		["Undo", "Ctrl Z"],
		["Redo", "Ctrl Shift Z", "Ctrl Y"]
	] as const;
	const selection = [
		["Add or remove a clip", "Ctrl click"],
		["Select a range", "Shift click"],
		["Select an area", "Drag on empty space"],
		["Select all", "Ctrl A"],
		["Clear the selection", "Esc"]
	] as const;
	const nudges = [
		["Move 1 frame", "Comma", "Period"],
		["Move 1 second", "Shift Comma", "Shift Period"]
	] as const;
	const view = [
		["Zoom at the pointer", "Ctrl wheel"],
		["Zoom at the playhead", "Ctrl +", "Ctrl -"],
		["Scroll", "Shift wheel"],
		["Fit all clips", "\\"]
	] as const;
</script>

<Section
	id="timeline-tracks"
	focus={{
		...on("tracks", "joint", "add-tracks"),
		marks: [
			{ kind: "label", at: pin(176, 652), text: "Alt + wheel" },
			{ kind: "arrow", from: pin(170, 644), to: pin(50, 644) }
		]
	}}
>
	<p>
		Video and audio clips sit on separate tracks. Add a track with +V or +A, and change its height
		with Alt and the mouse wheel.
	</p>
</Section>

<Section id="timeline-track-menu" focus={on("track-menu", "menu-joint", "menu")}>
	<p>
		To open this menu, right-click a track name or press T. Remove track also removes the clips on
		the track, and the last video track always stays.
	</p>
</Section>

<Section
	id="timeline-add"
	focus={{
		...on("drop", "editor-joint", "import"),
		marks: [{ kind: "label", at: pin(300, 712), text: "or drop files here" }]
	}}
>
	<p>
		A file you drop on the timeline lands on the track and time where you drop it, and overwrites
		the clips it covers. A video with sound also adds one audio clip for each of its audio tracks,
		and an image lasts 5 seconds.
	</p>
	<p>
		To swap the file of a clip, choose Replace in the clip menu. The clip keeps its place, its cuts
		and its settings.
	</p>
	<table>
		<tbody>
			<tr><th>Video</th><td><code>mp4 mkv mov webm avi m4v ts flv</code></td></tr>
			<tr><th>Audio</th><td><code>mp3 wav flac ogg opus m4a aac</code></td></tr>
			<tr><th>Images</th><td><code>png jpg jpeg webp bmp</code></td></tr>
		</tbody>
	</table>
</Section>

<Section id="timeline-select" focus={on("picked", "joint")}>
	<KeyList rows={selection} />
</Section>

<Section id="timeline-move" focus={on("moving", "joint")}>
	<p>
		Drag a clip to move it, even to another track. It stops at the next clip unless you hold Ctrl,
		which lets it overwrite the clips that it covers. Hold Alt to move the video and the audio of a
		file apart.
	</p>
	<KeyList rows={nudges} />
</Section>

<Section id="timeline-snapping" focus={on("snap", "joint", "snapping")}>
	<p>
		With snapping on, the edge of a clip sticks to the playhead, to the start of the timeline and to
		the edges of other clips. N turns it on or off, and Shift flips it for one drag.
	</p>
</Section>

<Section
	id="timeline-trim"
	focus={{
		...on("selected", "editor-joint", "cut-tools"),
		marks: [
			{ kind: "label", at: pin(161, 597), text: "I", centered: true },
			{ kind: "label", at: pin(189, 597), text: "O", centered: true },
			{ kind: "label", at: pin(330, 700), text: "drag an edge" },
			{ kind: "arrow", from: pin(445, 692), to: pin(522, 690) }
		]
	}}
>
	<p>
		Drag an edge of a clip, or move the playhead to where the clip should start or end and press I
		or O. X restores the full length.
	</p>
	<p>When two clips touch, the next clip follows the edge. Hold Alt to trim only one clip.</p>
</Section>

<Section
	id="timeline-split"
	focus={{
		...on("plain", "editor-joint", "split"),
		marks: [{ kind: "label", at: pin(217, 597), text: "S", centered: true }]
	}}
>
	<p>
		Split cuts every clip under the playhead in two. Each part is a clip of its own, so it can have
		<Term id="scope-clip">its own settings</Term>.
	</p>
</Section>

<Section
	id="timeline-delete"
	focus={{
		...on("gap", "joint", "gap"),
		marks: [{ kind: "arrow", from: pin(620, 690), to: pin(480, 690) }]
	}}
>
	<p>
		Delete removes the selected clips and leaves a gap. Shift Delete is a ripple delete, which also
		closes the gap. If you use ripple delete more often, Ripple delete by default in Settings swaps
		the two keys.
	</p>
	<p>
		To close a gap later, right-click it and choose Ripple delete. Edit &gt; Close gaps closes all
		of them at once.
	</p>
</Section>

<Section id="timeline-speed" focus={on("speed", "menu-joint", "speed")}>
	<p>
		Right-click a clip to change its speed, from 0.13x to 8x. With interpolation on, a slowed clip
		becomes smooth slow motion, because interpolation adds the frames that it lacks.
	</p>
	<p>For an audio clip, Gain in the same menu changes the volume, from -24 dB to +24 dB.</p>
</Section>

<Section id="timeline-fades" focus={on("fades", "menu-joint", "fades")}>
	<p>
		Drag a top corner of a clip to fade it in or out, for up to 5 seconds. To choose the curve of a
		fade, right-click near that edge of the clip. To set a crossfade, right-click between two clips
		that touch.
	</p>
</Section>

<Section
	id="timeline-zoom"
	focus={{
		...on("plain", "joint", "ruler"),
		marks: [
			{ kind: "label", at: pin(560, 716), text: "Ctrl + wheel" },
			{ kind: "arrow", from: pin(600, 702), to: pin(600, 664) }
		]
	}}
>
	<KeyList rows={view} />
</Section>

<Section id="timeline-keys" focus={on("selected", "editor-joint")}>
	<KeyList rows={keys} />
	<p>
		Step to a keyframe only works in <Term id="render-lossless" />.
	</p>
	<p>To see all hotkeys, choose Help &gt; Shortcuts.</p>
</Section>
