import { asset } from "$app/paths";
import type { AssetPath } from "$app/types";
import { stills } from "#lib/assets.ts";
import { fetchMedia } from "#lib/loader.ts";
import { required } from "#lib/required.ts";

export type Ui = {
	default(): Promise<unknown>;
	mount(
		canvasId: string,
		thumbs: Uint8Array,
		peaks: Uint8Array,
		frames: Uint8Array,
		dark: boolean,
		reduced: boolean
	): void;
	mount_dialog(canvasId: string, dark: boolean): void;
	unmount(): void;
	pause(paused: boolean): void;
	fit(): void;
	theme(dark: boolean): void;
	motion(reduced: boolean): void;
	open(section: string): void;
	scene(name: string): void;
	position(seconds: number): void;
	thumbnails(bytes: Uint8Array): void;
	configure(
		settings: string[],
		weights: Float32Array,
		source: number,
		output: number,
		clip: string,
		seconds: number
	): void;
};

async function pixels(file: string) {
	const bitmap = await createImageBitmap(await (await fetchMedia(file)).blob());
	const { width, height } = bitmap;
	const context = required(
		new OffscreenCanvas(width, height).getContext("2d", { willReadFrequently: true })
	);
	context.drawImage(bitmap, 0, 0);
	bitmap.close();
	return new Uint8Array(context.getImageData(0, 0, width, height).data.buffer);
}

const peaks = async () => new Uint8Array(await (await fetchMedia("wave.bin")).arrayBuffer());

const thumbsCache = new Map<string, Promise<Uint8Array>>();

export function loadThumbs(file: string) {
	let pending = thumbsCache.get(file);
	if (!pending) {
		pending = pixels(file).catch((error) => {
			thumbsCache.delete(file);
			throw error;
		});
		thumbsCache.set(file, pending);
	}
	return pending;
}

export async function loadUi() {
	const [ui, pictures, wave, frames]: [Ui, Uint8Array, Uint8Array, Uint8Array] = await Promise.all([
		import(asset("ui/interpolini_ui.js" as AssetPath)).then(async (module: Ui) => {
			await module.default();
			return module;
		}),
		loadThumbs("thumbs.webp"),
		peaks(),
		pixels(stills.file)
	]);
	return { ui, pictures, wave, frames };
}
