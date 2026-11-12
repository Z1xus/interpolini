import { bounds, type Mark } from "#lib/marks.ts";
import type { Show, View } from "#lib/show.ts";
import { aspect, regions, stages, within, type Rect, type Spot } from "#lib/window.ts";

export type Focus = Spot & {
	view?: View;
	show?: Show;
	frame?: number;
	marks?: readonly Mark[];
	tag?: (frame: number) => string;
};

const context = 0.12;
const close = { padding: 16, room: 0.03 };
const picture = { x: 0, y: 0, w: 1, h: 1 };

const contains = (outer: Rect, inner: Rect) =>
	inner.x >= outer.x &&
	inner.y >= outer.y &&
	inner.x + inner.w <= outer.x + outer.w &&
	inner.y + inner.h <= outer.y + outer.h;

const union = (a: Rect, b: Rect): Rect => {
	const [x, y] = [Math.min(a.x, b.x), Math.min(a.y, b.y)];
	return {
		x,
		y,
		w: Math.max(a.x + a.w, b.x + b.w) - x,
		h: Math.max(a.y + a.h, b.y + b.h) - y
	};
};

const overlap = (a: Rect, b: Rect) =>
	Math.max(0, Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x)) *
	Math.max(0, Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y));

const grown = ({ x, y, w, h }: Rect, room: number): Rect => ({
	x: x - room,
	y: y - room * aspect,
	w: w + 2 * room,
	h: h + 2 * room * aspect
});

const framed = ({ rect, mark, show, scene }: Focus, band: boolean) => {
	if (!band) {
		const target = mark ? union(rect, mark) : rect;
		if (scene && ("dialog" in stages[scene] || "menu" in stages[scene]))
			return { target, margin: 0, padding: close.padding };
		return { target, margin: context, padding: 0 };
	}
	if (show && contains(rect, regions.preview))
		return { target: regions.preview, margin: 0, padding: 0 };
	if (show && contains(regions.preview, rect)) return { target: rect, margin: 0, padding: 0 };
	return { target: grown(mark ?? rect, close.room), margin: 0, padding: close.padding };
};

const clamped = (center: number, visible: number, slack: number, from: number, size: number) =>
	Math.min(Math.max(center, from + visible / 2 - slack), from + size - visible / 2 + slack);

const centered = (center: number, visible: number, slack: number, from: number, size: number) => {
	if (visible < size) return clamped(center, visible, slack, from, size);
	return visible < 1 ? clamped(center, visible, slack, 0, 1) : 0.5;
};

export const reach = (pixels: number) => pixels / regions.preview.w;

export function camera(focus: Focus, band: boolean, width: number, height: number, pixels: number) {
	if (!width || !height) return { scale: 1, x: 0, y: 0, mark: focus.mark };
	const base = Math.min(width / height > aspect ? height * aspect : width, reach(pixels));
	const baseHeight = base / aspect;
	const { target, margin, padding } = framed(focus, band);
	const zoom = (rect: Rect) =>
		Math.max(
			Math.min(
				(width - 2 * padding) / ((rect.w + 2 * margin) * base),
				(height - 2 * padding) / ((rect.h + 2 * margin) * baseHeight),
				reach(pixels) / base
			),
			1
		);
	const marks = band ? focus.marks : undefined;
	const apart =
		marks && !contains(regions.preview, target) && contains(picture, bounds(marks, [0, 0]));
	let rect = target;
	let scale = zoom(rect);
	for (let pass = 0; marks && !apart && pass < 2; pass++) {
		const pixel = [
			1 / (scale * base * regions.preview.w),
			1 / (scale * baseHeight * regions.preview.h)
		] as const;
		rect = union(target, within(regions.preview, bounds(marks, pixel)));
		scale = zoom(rect);
	}
	const [visibleX, visibleY] = [width / (scale * base), height / (scale * baseHeight)];
	const hold = focus.show && contains(regions.preview, rect) ? regions.preview : regions.window;
	const x = centered(rect.x + rect.w / 2, visibleX, padding / (scale * base), hold.x, hold.w);
	const y = centered(rect.y + rect.h / 2, visibleY, padding / (scale * baseHeight), hold.y, hold.h);
	const view = { x: x - visibleX / 2, y: y - visibleY / 2, w: visibleX, h: visibleY };
	const { mark } = focus;
	return {
		scale,
		x: (0.5 - x) * scale * base,
		y: (0.5 - y) * scale * baseHeight,
		mark: mark && overlap(mark, view) >= (mark.w * mark.h) / 2 ? mark : undefined
	};
}
