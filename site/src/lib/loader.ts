import { asset } from "$app/paths";
import type { AssetPath } from "$app/types";
import { isStill, moment, type AssetName } from "#lib/assets.ts";

export type Resource = ImageBitmap | HTMLVideoElement;

export const sizeOf = (resource: Resource) =>
	resource instanceof HTMLVideoElement
		? { width: resource.videoWidth, height: resource.videoHeight }
		: { width: resource.width, height: resource.height };

const url = (file: string) => asset(`media/${file}` as AssetPath);

export async function fetchMedia(file: string, signal?: AbortSignal) {
	const response = await fetch(url(file), { signal });
	if (!response.ok) throw new Error(`${file}: ${response.status}`);
	return response;
}

async function loadImage(file: string, signal: AbortSignal) {
	return createImageBitmap(await (await fetchMedia(file, signal)).blob());
}

const loadGrid = (name: string, signal: AbortSignal, lost: () => void) =>
	new Promise<HTMLVideoElement>((resolve, reject) => {
		const video = document.createElement("video");
		video.muted = true;
		video.loop = true;
		video.playsInline = true;
		video.preload = "auto";
		video.onloadeddata = () => {
			video.onerror = () => {
				console.error(video.error);
				lost();
			};
			resolve(video);
		};
		video.onerror = () => reject(video.error);
		signal.onabort = () => {
			release(video);
			reject(signal.reason);
		};
		video.src = url(`${name}.webm`);
	});

export function load(name: AssetName, signal: AbortSignal, lost: () => void) {
	if (name === "moment") return loadImage(moment.file, signal);
	return isStill(name) ? loadImage(`${name}.avif`, signal) : loadGrid(name, signal, lost);
}

export function release(resource: Resource) {
	if (resource instanceof ImageBitmap) {
		resource.close();
		return;
	}
	resource.removeAttribute("src");
	resource.load();
}
