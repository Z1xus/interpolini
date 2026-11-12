import type { Rect } from "#lib/window.ts";

type Point = readonly [x: number, y: number];

export type Mark =
	| { kind: "arrow"; from: Point; to: Point }
	| { kind: "ellipse"; at: Point; rx: number; ry: number }
	| { kind: "outline"; rect: Rect }
	| { kind: "label"; at: Point; text: string; centered?: true };

const label = { glyph: 8, padding: 14, above: 18, below: 4 };

export function bounds(marks: readonly Mark[], [pixelX, pixelY]: Point): Rect {
	const points = marks.flatMap((mark): Point[] => {
		switch (mark.kind) {
			case "arrow":
				return [mark.from, mark.to];
			case "ellipse":
				return [
					[mark.at[0] - mark.rx, mark.at[1] - mark.ry],
					[mark.at[0] + mark.rx, mark.at[1] + mark.ry]
				];
			case "outline":
				return [
					[mark.rect.x, mark.rect.y],
					[mark.rect.x + mark.rect.w, mark.rect.y + mark.rect.h]
				];
			case "label": {
				const wide = (mark.text.length * label.glyph + label.padding) * pixelX;
				const start = mark.at[0] - (mark.centered ? wide / 2 : 0);
				return [
					[start, mark.at[1] - label.above * pixelY],
					[start + wide, mark.at[1] + label.below * pixelY]
				];
			}
		}
	});
	const xs = points.map(([x]) => x);
	const ys = points.map(([, y]) => y);
	const [x, y] = [Math.min(...xs), Math.min(...ys)];
	return { x, y, w: Math.max(...xs) - x, h: Math.max(...ys) - y };
}

type Drawn = { path: string } | { text: string; x: number; y: number; centered?: true };

const wobble = (seed: number, step: number) => {
	const value = Math.sin(seed * 12.9898 + step * 78.233) * 43758.5453;
	return value - Math.floor(value) - 0.5;
};

const round = (value: number) => value.toFixed(1);

const move = (command: string, x: number, y: number) => `${command}${round(x)} ${round(y)}`;

function arrow(from: Point, to: Point, seed: number) {
	const length = Math.hypot(to[0] - from[0], to[1] - from[1]);
	const bend = length * 0.14 * (seed % 2 === 0 ? 1 : -1);
	const control: Point = [
		(from[0] + to[0]) / 2 - ((to[1] - from[1]) / length) * bend,
		(from[1] + to[1]) / 2 + ((to[0] - from[0]) / length) * bend
	];
	const heading = Math.atan2(to[1] - control[1], to[0] - control[0]);
	const head = Math.min(length * 0.3, 22);
	const barb = (side: number, step: number) => {
		const angle = heading + Math.PI + side * (0.5 + wobble(seed, step) * 0.2);
		return move("L", to[0] + Math.cos(angle) * head, to[1] + Math.sin(angle) * head);
	};
	return [
		move("M", from[0], from[1]),
		`Q${round(control[0])} ${round(control[1])} ${round(to[0])} ${round(to[1])}`,
		barb(1, 1),
		move("M", to[0], to[1]),
		barb(-1, 2)
	].join("");
}

function loop(cx: number, cy: number, rx: number, ry: number, seed: number) {
	const steps = 40;
	const start = wobble(seed, 0) * Math.PI;
	const turn = 2 * Math.PI * 1.1;
	let path = "";
	for (let step = 0; step <= steps; step++) {
		const angle = start + (turn * step) / steps;
		const grown = 1 + 0.06 * (step / steps) + 0.03 * wobble(seed, step + 3);
		const x = cx + Math.cos(angle) * rx * grown;
		const y = cy + Math.sin(angle) * ry * grown;
		path += move(step === 0 ? "M" : "L", x, y);
	}
	return path;
}

function outline({ x, y, w, h }: Rect, width: number, height: number) {
	return `${move("M", x * width, y * height)}h${round(w * width)}v${round(h * height)}h${round(-w * width)}Z`;
}

export function draw(mark: Mark, width: number, height: number, seed: number): Drawn {
	const at = ([x, y]: Point): Point => [x * width, y * height];
	switch (mark.kind) {
		case "arrow":
			return { path: arrow(at(mark.from), at(mark.to), seed) };
		case "ellipse": {
			const [cx, cy] = at(mark.at);
			return { path: loop(cx, cy, mark.rx * width, mark.ry * height, seed) };
		}
		case "outline":
			return { path: outline(mark.rect, width, height) };
		case "label": {
			const [x, y] = at(mark.at);
			return { text: mark.text, x, y, centered: mark.centered };
		}
	}
}
