import type { Mark } from "#lib/marks.ts";
import { regions, within } from "#lib/window.ts";

export const middle = within(regions.preview, { x: 0.2, y: 0.2, w: 0.6, h: 0.6 });

export const left = within(regions.preview, { x: 0, y: 0.2, w: 0.6, h: 0.6 });

export const wallMotion: Mark[] = [{ kind: "arrow", from: [0.58, 0.2], to: [0.37, 0.42] }];

export const wallEdge: Mark[] = [{ kind: "arrow", from: [0.86, 0.2], to: [0.65, 0.4] }];

export const bladeEdge: Mark[] = [{ kind: "arrow", from: [0.2, 0.28], to: [0.39, 0.48] }];

export const swordTip: Mark[] = [{ kind: "ellipse", at: [0.53, 0.52], rx: 0.17, ry: 0.44 }];

export const ghostSword: Mark[] = [{ kind: "ellipse", at: [0.3, 0.36], rx: 0.22, ry: 0.28 }];

export const stairs: Mark[] = [{ kind: "arrow", from: [0.2, 0.16], to: [0.52, 0.3] }];

export const wallCopy: Mark[] = [{ kind: "arrow", from: [0.92, 0.8], to: [0.7, 0.55] }];

export const wallGrain: Mark[] = [{ kind: "ellipse", at: [0.15, 0.45], rx: 0.13, ry: 0.42 }];

export const grass: Mark[] = [{ kind: "ellipse", at: [0.76, 0.5], rx: 0.18, ry: 0.34 }];

export const chatLines: Mark[] = [
	{ kind: "outline", rect: { x: 0.004, y: 0.26, w: 0.5, h: 0.52 } }
];

export const chatGhost: Mark[] = [
	{ kind: "outline", rect: { x: 0.004, y: 0.42, w: 0.33, h: 0.36 } }
];

export const softSeam: Mark[] = [{ kind: "outline", rect: { x: 0.004, y: 0.3, w: 0.38, h: 0.3 } }];
