import { chatMask } from "#lib/assets.ts";
import layout from "#lib/layout.json";

export type Rect = { x: number; y: number; w: number; h: number };

const [width, height] = layout.window;

export const aspect = width / height;

const fraction = ([x, y, w, h]: number[]): Rect => ({
	x: x / width,
	y: y / height,
	w: w / width,
	h: h / height
});

const fractions = <T extends Record<string, number[]>>(rects: T) =>
	Object.fromEntries(Object.entries(rects).map(([name, rect]) => [name, fraction(rect)])) as Record<
		keyof T,
		Rect
	>;

export const regions = {
	window: fraction([0, 0, width, height]),
	...fractions(layout.base),
	dedup: fractions(layout.dedup),
	pre: fractions(layout.pre),
	interpolation: fractions(layout.interpolation),
	rife: fractions(layout.rife),
	peel: fractions(layout.peel),
	still: fractions(layout.still),
	image: fractions(layout.image),
	blending: fractions(layout.blending),
	color: fractions(layout.color),
	upscale: fractions(layout.upscale),
	output: fractions(layout.output),
	size: fractions(layout.size)
};

export const stages = Object.fromEntries(
	Object.entries(layout.scenes).map(([scene, rects]) => [scene, fractions(rects)])
) as { [S in keyof typeof layout.scenes]: Record<keyof (typeof layout.scenes)[S], Rect> };

export type Scene = keyof typeof stages;

export const panels = {
	dedup: { open: "dedup", settings: [] },
	pre: { open: "pre", settings: ["pre-interpolation.enabled=yes"] },
	interpolation: { open: "interpolation", settings: [] },
	rife: { open: "interpolation", settings: ["interpolation.engine=rife"] },
	peel: { open: "masking", settings: ["mask.method=peel"] },
	still: { open: "masking", settings: ["mask.method=still"] },
	image: { open: "masking", settings: ["mask.method=image", `mask.image=${chatMask.name}`] },
	blending: { open: "blending", settings: [] },
	color: { open: "color", settings: [] },
	upscale: { open: "upscale", settings: [] },
	output: { open: "output", settings: [] },
	size: { open: "output", settings: ["output.target=size"] }
} as const;

type Panel = keyof typeof panels;

type Rows<P extends Panel> = Exclude<keyof (typeof regions)[P], "section">;

export type Spot = { rect: Rect; mark?: Rect; panel?: Panel; scene?: Scene };

export const on = <S extends Scene>(
	scene: S,
	rect: keyof (typeof stages)[S],
	mark?: keyof (typeof stages)[S]
): Spot => {
	const group: Record<string, Rect> = stages[scene];
	return { scene, rect: group[rect as string], mark: mark && group[mark as string] };
};

export const at = <P extends Panel>(panel: P, row?: Rows<P>): Spot => {
	const group: Record<string, Rect> = regions[panel];
	return { panel, rect: group.section, mark: row && group[row as string] };
};

export const beside = <P extends Panel>(panel: P, row: Rows<P>): Spot => ({
	...at(panel, row),
	rect: regions.window
});

export const within = (region: Rect, inner: Rect): Rect => ({
	x: region.x + inner.x * region.w,
	y: region.y + inner.y * region.h,
	w: inner.w * region.w,
	h: inner.h * region.h
});

export const pin = (x: number, y: number) =>
	[
		(x / width - regions.preview.x) / regions.preview.w,
		(y / height - regions.preview.y) / regions.preview.h
	] as const;

const detail = (left: number, top: number, size: number): Rect => {
	const { w, h } = regions.preview;
	return within(regions.preview, { x: left, y: top, w: size, h: (size * w) / h });
};

export const details = {
	picture: detail(0.25, 0.3, 0.4)
};
