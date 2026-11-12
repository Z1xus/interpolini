import { asset } from "$app/paths";
import type { AssetPath } from "$app/types";
import { stills } from "#lib/assets.ts";
import { fetchMedia } from "#lib/loader.ts";
import { required } from "#lib/required.ts";

export type Ui = {
	default(): Promise<unknown>;
	mount(canvasId: string, thumbs: Uint8Array, peaks: Uint8Array, frames: Uint8Array): void;
	mount_dialog(canvasId: string): void;
	unmount(): void;
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
	const context = required(new OffscreenCanvas(width, height).getContext("2d"));
	context.drawImage(bitmap, 0, 0);
	bitmap.close();
	return new Uint8Array(context.getImageData(0, 0, width, height).data.buffer);
}

const peaks = async () => new Uint8Array(await (await fetchMedia("wave.bin")).arrayBuffer());

export async function loadUi() {
	const [ui, pictures, sword, wave, frames]: [Ui, Uint8Array, Uint8Array, Uint8Array, Uint8Array] =
		await Promise.all([
			import(asset("ui/interpolini_ui.js" as AssetPath)),
			pixels("thumbs.webp"),
			pixels("thumbs-sword.webp"),
			peaks(),
			pixels(stills.file)
		]);
	await ui.default();
	return { ui, pictures, wave, frames, thumbs: { [stills.clip]: pictures, "240.mp4": sword } };
}
