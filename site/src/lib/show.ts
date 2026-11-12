import {
	gridOf,
	gridRate,
	isStill,
	moment,
	rateOf,
	renderOf,
	sourceRate,
	variantNames,
	type AssetName,
	type GridName
} from "#lib/assets.ts";
import { planned, type Weighting } from "#lib/weights.ts";
import type { Scene, Spot } from "#lib/window.ts";

export type View = "normal" | "difference";

export type Blend = {
	fps: number;
	intensity: number;
	gamma: number;
	weighting: Weighting;
	stride: number;
};

export type Show = { blend: Blend } | { grid: GridName; variant: string };

type Choice = { label: string; show?: Show; view?: View; scene?: Scene; spot?: Spot };

export type Control =
	| { kind: "choose"; label: string; options: Choice[]; initial?: string }
	| {
			kind: "range";
			label: string;
			field: "fps" | "intensity" | "gamma";
			min: number;
			max: number;
			step: number;
	  }
	| { kind: "frames"; label: string; from: number; to: number };

const defaults: Blend = {
	fps: 60,
	intensity: 1,
	gamma: 1,
	weighting: "equal",
	stride: 1
};

export const blended = (blend: Partial<Blend> = {}): Show => ({ blend: { ...defaults, ...blend } });

export const tile = (grid: GridName, variant: string): Show => ({ grid, variant });

export const choices = (grid: GridName): Choice[] =>
	variantNames(grid).map((variant) => ({ label: variant, show: tile(grid, variant) }));

export const variants = (grid: GridName, label: string, initial?: string): Control => ({
	kind: "choose",
	label,
	initial,
	options: choices(grid)
});

export const differences = (grid: GridName, label: string, variant: string): Control => ({
	kind: "choose",
	label,
	options: [
		...choices(grid),
		{ label: "difference", show: tile(grid, variant), view: "difference" }
	]
});

export const assetOf = (show: Show): AssetName => ("blend" in show ? "moment" : show.grid);

export const sourceTimeOf = (show: Show, frame: number) => {
	if ("blend" in show) return moment.start + frame / moment.rate;
	const grid = gridOf(show.grid);
	return grid.span[0] + (grid.still ?? frame) / gridRate + (grid.offset ?? 0);
};

export function keyFrameOf(show?: Show) {
	if (!show) return 0;
	if ("blend" in show) return moment.center;
	if (isStill(show.grid)) return 0;
	const [from, to] = gridOf(show.grid).span;
	return Math.floor(((to - from) * gridRate) / 2);
}

export const settingsOf = (show: Show) => {
	if (!("blend" in show)) {
		const settings = gridOf(show.grid).variants.find(([variant]) => variant === show.variant)?.[1];
		if (settings === null)
			return [
				"dedup.enabled=no",
				"pre-interpolation.enabled=no",
				"interpolation.enabled=no",
				"mask.enabled=no",
				"blending.enabled=no"
			];
		return settings ? renderOf(show.grid, settings) : [];
	}
	const { fps, intensity, gamma, weighting, stride } = show.blend;
	return [
		`blending.fps=${fps}`,
		`blending.intensity=${intensity}`,
		`blending.gamma=${gamma}`,
		`blending.weighting=${weighting}`,
		`interpolation.fps=${moment.rate / stride}`
	];
};

export const planOf = (settings: readonly string[], show?: Show) => {
	if (show && "grid" in show) {
		const original =
			gridOf(show.grid).variants.find(([name]) => name === show.variant)?.[1] === null;
		return planned(settings, original ? sourceRate : rateOf(show.grid));
	}
	return planned(settings, sourceRate);
};

export function initialOf(control: Control | undefined, show: Show | undefined, frame: number) {
	if (control?.kind === "choose") {
		return Math.max(
			control.options.findIndex(({ label }) => label === control.initial),
			0
		);
	}
	if (control?.kind === "frames") return frame;
	if (control?.kind !== "range" || !show || !("blend" in show)) return 0;
	return show.blend[control.field];
}

export function labelOf(control: Control, selected: number) {
	if (control.kind === "choose") return control.options[selected].label;
	return `${selected}`;
}

const decimal = /^-?\d+(\.\d+)?$/;

export function selectedOf(control: Control, text: string) {
	if (control.kind === "choose") {
		const index = control.options.findIndex(({ label }) => label === text);
		return index < 0 ? undefined : index;
	}
	if (!decimal.test(text)) return undefined;
	const value = Number(text);
	if (control.kind === "range") {
		const steps = (value - control.min) / control.step;
		const onStep = Math.abs(steps - Math.round(steps)) < 1e-9;
		if (onStep && value >= control.min && value <= control.max) return value;
	}
	if (
		control.kind === "frames" &&
		Number.isInteger(value) &&
		value >= control.from &&
		value <= control.to
	) {
		return value;
	}
}
