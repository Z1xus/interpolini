export const weightings = [
	"equal",
	"gaussian",
	"gaussian_sym",
	"pyramid",
	"ascending",
	"descending",
	"vegas",
	"custom"
] as const;

export type Weighting = (typeof weightings)[number];

const customCurve = [1, 2, 5];

const spread = (count: number, start: number, end: number) =>
	Array.from({ length: count }, (_, x) => (x * (end - start)) / Math.max(count - 1, 1) + start);

const gaussian = (count: number, mean: number, start: number, end: number) =>
	spread(count, start, end).map((x) => Math.exp(-((x - mean) ** 2) / 2));

const vegas = (ratio: number) => {
	const factor = Math.trunc(ratio);
	if (factor % 2 === 1) return Array<number>(factor).fill(1);
	return Array.from({ length: factor + 1 }, (_, x) => (x === 0 || x === factor ? 1 : 2));
};

const raw = (ratio: number, weighting: Weighting) => {
	const count = Math.round(ratio) | 1;
	const half = (count - 1) / 2;
	switch (weighting) {
		case "equal":
			return Array<number>(count).fill(1);
		case "gaussian":
			return gaussian(count, 2, 0, 2);
		case "gaussian_sym":
			return gaussian(count, 0, -2, 2);
		case "pyramid":
			return Array.from({ length: count }, (_, x) => half - Math.abs(x - half) + 1);
		case "ascending":
			return Array.from({ length: count }, (_, x) => x + 1);
		case "descending":
			return Array.from({ length: count }, (_, x) => count - x);
		case "vegas":
			return vegas(ratio);
		case "custom":
			return spread(count, 0, customCurve.length - 0.1).map((x) => customCurve[Math.trunc(x)]);
	}
};

export function plan(ratio: number, weighting: Weighting) {
	if (Math.round(ratio) <= 1) return [1];
	const weights = raw(ratio, weighting);
	const total = weights.reduce((sum, weight) => sum + weight, 0);
	return weights.map((weight) => weight / total);
}

const rateAt = (text: string, from: number) =>
	text.endsWith("x") ? Math.round(from * parseFloat(text)) : Number(text);

export function planned(settings: readonly string[], source: number) {
	const read = (key: string, fallback: string) =>
		settings.findLast((entry) => entry.startsWith(`${key}=`))?.slice(key.length + 1) ?? fallback;
	const interpolating = read("interpolation.enabled", "yes") === "yes";
	let rate = source;
	const pre = rateAt(read("pre-interpolation.fps", "2x"), rate);
	if (
		read("pre-interpolation.enabled", "no") === "yes" &&
		pre > rate &&
		!(interpolating && read("interpolation.engine", "svp") === "rife")
	) {
		rate = pre;
	}
	const main = rateAt(read("interpolation.fps", "960"), rate);
	if (interpolating && main > rate) rate = main;
	const fps = Number(read("blending.fps", "60"));
	if (read("blending.enabled", "yes") !== "yes" || fps >= rate)
		return { source, output: rate, weights: [1] };
	const intensity = Number(read("blending.intensity", "1"));
	return {
		source,
		output: fps,
		weights: plan((rate / fps) * intensity, read("blending.weighting", "equal") as Weighting)
	};
}

const unit = 1 << 15;

export function fixed(weights: number[]) {
	const scaled = weights.map((weight) => Math.round(weight * unit));
	scaled[scaled.length >> 1] += unit - scaled.reduce((sum, weight) => sum + weight, 0);
	return Float32Array.from(scaled);
}
