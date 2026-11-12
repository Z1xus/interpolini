<script lang="ts">
	import Section from "#lib/components/Section.svelte";
	import { on, stages, within, type Rect, type Spot } from "#lib/window.ts";

	const { dialog, projects, autosave } = stages["projects-settings"];
	const row = (mark: Rect): Spot => ({
		scene: "projects-settings",
		rect: within(dialog, { x: 0, y: 0, w: 1, h: 0.4 }),
		mark: { ...mark, w: mark.w * 0.65 }
	});
</script>

<Section id="projects-enable" focus={row(projects)}>
	<p>
		Project files are off by default. Turn on Project files in Settings &gt; Projects. The Project
		items then appear in the File menu.
	</p>
</Section>

<Section id="projects-save" focus={on("file-menu", "menu", "file-save")}>
	<p>
		Save, or Ctrl S, writes a <code>.lini</code> file. Save as, or Ctrl Shift S, asks for a new name.
		The file holds the timeline, the settings of each clip and the view.
	</p>
	<p>
		It holds the paths of the source clips, not the clips themselves, so if you move them, choose
		Locate missing files in the File menu.
	</p>
</Section>

<Section id="projects-open" focus={on("file-menu", "menu", "file-open")}>
	<p>
		New project is Ctrl N and Open project is Ctrl O. You can also drop a <code>.lini</code>
		file on the window. With unsaved changes, interpolini asks first when Ask to save in Settings is on.
	</p>
</Section>

<Section id="projects-recent" focus={on("recent-menu", "menu", "recent")}>
	<p>
		Open recent lists the last 8 projects. Clear list empties it. Reopen last project in Settings
		opens the latest project at start.
	</p>
</Section>

<Section id="projects-autosave" focus={row(autosave)}>
	<p>
		Autosave writes a recovery copy when the project has unsaved changes. Set it to Off, 1, 5 or 10
		minutes. The copy is next to the project file, with <code>.rec</code> at the end of the name, or next
		to the app when the project has no file. The project file stays as you saved it.
	</p>
</Section>

<Section id="projects-recover" focus={on("recover", "modal")}>
	<p>
		When a recovery copy is left, interpolini offers it at the next start, for example after a
		crash. Recover opens the changes. Discard deletes the copy. Later keeps it.
	</p>
</Section>

<Section id="projects-settings" focus={on("file-menu", "menu", "file-project")}>
	<p>
		Project settings sets the Resolution, Frame rate and Background of the project. New clips can
		fit the canvas, fill it or keep their size.
	</p>
</Section>
