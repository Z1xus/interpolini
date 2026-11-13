import { resolve } from "$app/paths";
import type { RouteId } from "$app/types";

type Slug<T extends string = RouteId> = T extends `/${infer Name}` ? Name : never;

export const origin = "https://z1xus.github.io";

type Entry = { id: string; title: string };
export type Page = {
	title: string;
	slug: Slug;
	description: string;
	sections: readonly Entry[];
};
type Group = { title: string; pages: readonly Page[] };

const list = [
	{
		title: "Basics",
		pages: [
			{
				title: "interpolini",
				slug: "",
				description: "Frame interpolation and motion blur for video.",
				sections: [
					{ id: "overview", title: "Motion blur" },
					{ id: "download", title: "Download" }
				]
			},
			{
				title: "Get started",
				slug: "get-started",
				description: "Render a clip.",
				sections: [
					{ id: "add-clips", title: "Add clips" },
					{ id: "check-settings", title: "Check the settings" },
					{ id: "render", title: "Render" }
				]
			},
			{
				title: "Concepts",
				slug: "concepts",
				description: "Interpolation, blending and artifacts.",
				sections: [
					{ id: "interpolation", title: "Interpolation" },
					{ id: "blending", title: "Blending" },
					{ id: "artifacts", title: "Artifacts" },
					{ id: "fighting-artifacts", title: "Reducing artifacts" }
				]
			}
		]
	},
	{
		title: "Editing",
		pages: [
			{
				title: "Timeline",
				slug: "timeline",
				description: "Arrange, cut and move clips.",
				sections: [
					{ id: "timeline-tracks", title: "Tracks" },
					{ id: "timeline-track-menu", title: "Track menu" },
					{ id: "timeline-add", title: "Add clips" },
					{ id: "timeline-select", title: "Select clips" },
					{ id: "timeline-move", title: "Move clips" },
					{ id: "timeline-snapping", title: "Snapping" },
					{ id: "timeline-trim", title: "Trim" },
					{ id: "timeline-split", title: "Split" },
					{ id: "timeline-delete", title: "Remove and ripple delete" },
					{ id: "timeline-speed", title: "Clip speed" },
					{ id: "timeline-fades", title: "Fades and crossfades" },
					{ id: "timeline-zoom", title: "Zoom and scroll" },
					{ id: "timeline-keys", title: "Keyboard" }
				]
			},
			{
				title: "Preview",
				slug: "preview",
				description: "Compare, zoom and transform.",
				sections: [
					{ id: "preview-views", title: "Views" },
					{ id: "preview-hold", title: "Hold to compare" },
					{ id: "preview-zoom", title: "Zoom and pan" },
					{ id: "preview-playback", title: "Playback resolution" },
					{ id: "preview-frame", title: "Save frame" },
					{ id: "preview-transform", title: "Transform" },
					{ id: "preview-move", title: "Move" },
					{ id: "preview-scale", title: "Scale" },
					{ id: "preview-crop", title: "Crop" },
					{ id: "preview-stretch", title: "Stretch" }
				]
			},
			{
				title: "Settings scope",
				slug: "settings-scope",
				description: "Change settings for all clips or for one clip.",
				sections: [
					{ id: "scope-all", title: "All clips" },
					{ id: "scope-clip", title: "This clip" },
					{ id: "scope-config", title: "Configs" },
					{ id: "scope-share", title: "Share" }
				]
			},
			{
				title: "Projects",
				slug: "projects",
				description: "Save, open and recover your timeline.",
				sections: [
					{ id: "projects-enable", title: "Turn on project files" },
					{ id: "projects-save", title: "Save" },
					{ id: "projects-open", title: "Open" },
					{ id: "projects-recent", title: "Recent projects" },
					{ id: "projects-autosave", title: "Autosave" },
					{ id: "projects-recover", title: "Recover" },
					{ id: "projects-settings", title: "Project settings" }
				]
			},
			{
				title: "Render",
				slug: "render",
				description: "Make a file from the timeline.",
				sections: [
					{ id: "render-button", title: "Render" },
					{ id: "render-separate", title: "Export each clip separately" },
					{ id: "render-lossless", title: "Lossless cut" },
					{ id: "render-folder", title: "Render folder" }
				]
			}
		]
	},
	{
		title: "Settings",
		pages: [
			{
				title: "Dedup",
				slug: "dedup-settings",
				description: "Settings in the Dedup section.",
				sections: [
					{ id: "dedup-section", title: "Dedup section" },
					{ id: "dedup-repeats", title: "Repeats replaced" }
				]
			},
			{
				title: "Pre-interpolation",
				slug: "pre-interpolation-settings",
				description: "Settings in the Pre-interpolation section.",
				sections: [
					{ id: "pre-section", title: "Pre-interpolation section" },
					{ id: "pre-fps", title: "Frame rate" },
					{ id: "pre-backend", title: "Backend" },
					{ id: "pre-model", title: "Model" }
				]
			},
			{
				title: "Interpolation",
				slug: "interpolation-settings",
				description: "Settings in the Interpolation section.",
				sections: [
					{ id: "interp-section", title: "Interpolation section" },
					{ id: "interp-engine", title: "Engine" },
					{ id: "interp-fps", title: "Frame rate" },
					{ id: "interp-speed", title: "Speed" },
					{ id: "interp-tuning", title: "Tuning" },
					{ id: "interp-look", title: "Look" },
					{ id: "interp-half", title: "Half-size analysis" },
					{ id: "interp-rife", title: "Backend and Model" }
				]
			},
			{
				title: "Masking",
				slug: "masking-settings",
				description: "Settings in the Masking section.",
				sections: [
					{ id: "masking-section", title: "Masking section" },
					{ id: "mask-method", title: "Method" },
					{ id: "mask-peel", title: "Peel limit" },
					{ id: "mask-still", title: "Still limit" },
					{ id: "mask-edge", title: "Edge" },
					{ id: "mask-tolerance", title: "Tolerance" },
					{ id: "mask-gallery", title: "Mask gallery" },
					{ id: "mask-soft", title: "Soft edge" },
					{ id: "mask-editor", title: "Mask editor" },
					{ id: "mask-keys", title: "Mask editor keys" }
				]
			},
			{
				title: "Blending",
				slug: "blending-settings",
				description: "Settings in the Blending section.",
				sections: [
					{ id: "blending-section", title: "Blending section" },
					{ id: "blending-fps", title: "Frame rate" },
					{ id: "blending-intensity", title: "Intensity" },
					{ id: "blending-gamma", title: "Gamma" },
					{ id: "blending-weighting", title: "Weighting" }
				]
			},
			{
				title: "Color",
				slug: "color-settings",
				description: "Settings in the Color section.",
				sections: [
					{ id: "color-section", title: "Color section" },
					{ id: "color-brightness", title: "Brightness" },
					{ id: "color-contrast", title: "Contrast" },
					{ id: "color-saturation", title: "Saturation" },
					{ id: "color-hue", title: "Hue" },
					{ id: "color-lut", title: "LUT" }
				]
			},
			{
				title: "Upscale",
				slug: "upscale-settings",
				description: "Settings in the Upscale section.",
				sections: [
					{ id: "upscale-section", title: "Upscale section" },
					{ id: "upscale-resolution", title: "Resolution" },
					{ id: "upscale-method", title: "Method" }
				]
			},
			{
				title: "Output",
				slug: "output-settings",
				description: "Settings in the Output section.",
				sections: [
					{ id: "output-section", title: "Output section" },
					{ id: "output-codec", title: "Codec" },
					{ id: "output-encoder", title: "Encoder" },
					{ id: "output-target", title: "Target" },
					{ id: "output-quality", title: "Quality" },
					{ id: "output-size", title: "Size" },
					{ id: "output-effort", title: "Effort" },
					{ id: "output-audio", title: "Audio tracks" },
					{ id: "output-options", title: "Options" },
					{ id: "output-container", title: "Container" }
				]
			}
		]
	},
	{
		title: "Setup",
		pages: [
			{
				title: "Install",
				slug: "install",
				description: "Trust, Send to on Windows and the app menu on Linux.",
				sections: [
					{ id: "trust", title: "Trust" },
					{ id: "send-to", title: "Send to on Windows" },
					{ id: "linux-menu", title: "App menu on Linux" }
				]
			},
			{
				title: "Components",
				slug: "components",
				description: "RIFE models, TensorRT and FFmpeg.",
				sections: [
					{ id: "components-open", title: "Open Components" },
					{ id: "components-rife", title: "RIFE models" },
					{ id: "components-tensorrt", title: "TensorRT" },
					{ id: "components-ffmpeg", title: "FFmpeg" }
				]
			}
		]
	},
	{
		title: "Reference",
		pages: [
			{
				title: "CLI and config",
				slug: "cli",
				description: "Render without the window and keep settings in files.",
				sections: [
					{ id: "cli-usage", title: "Usage" },
					{ id: "cli-settings", title: "Settings" },
					{ id: "cli-range", title: "Part of a clip" },
					{ id: "cli-batch", title: "Batch and scripts" },
					{ id: "cli-wrap", title: "Wrap the CLI" },
					{ id: "cli-picker", title: "File picker" },
					{ id: "config-file", title: "interpolini.ini" },
					{ id: "config-lookup", title: "Lookup order" }
				]
			},
			{
				title: "Elsewhere",
				slug: "elsewhere",
				description: "Related tools and where to read about them.",
				sections: [
					{ id: "elsewhere-recording", title: "Recording" },
					{ id: "elsewhere-sharing", title: "Sharing" },
					{ id: "elsewhere-youtube", title: "YouTube" },
					{ id: "elsewhere-ffmpeg", title: "FFmpeg" }
				]
			},
			{
				title: "Credits",
				slug: "credits",
				description: "Who made interpolini and what it is built on.",
				sections: [
					{ id: "credits-author", title: "Author" },
					{ id: "credits-thanks", title: "Thanks" },
					{ id: "credits-projects", title: "Built on" }
				]
			}
		]
	}
] as const satisfies readonly Group[];

export const groups: readonly Group[] = list;

export type SectionId = (typeof list)[number]["pages"][number]["sections"][number]["id"];

export const pages = groups.flatMap((group) => group.pages);

export const route = (page: Page): RouteId => `/${page.slug}`;

const sections = new Map<string, { page: Page; title: string }>();
for (const page of pages) {
	for (const { id, title } of page.sections) {
		if (sections.has(id)) throw new Error(`Section id "${id}" is used twice`);
		sections.set(id, { page, title });
	}
}

export function locate(id: SectionId) {
	const found = sections.get(id);
	if (!found) throw new Error(`There is no section "${id}"`);
	return { href: `${resolve(route(found.page))}#${id}`, title: found.title };
}
