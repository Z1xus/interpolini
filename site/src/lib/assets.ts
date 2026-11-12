type Crop = { width: number; height: number; x: number; y: number };

const world: Crop = { width: 1280, height: 720, x: 640, y: 360 };
const chat: Crop = { width: 480, height: 270, x: 0, y: 810 };
const wall: Crop = { width: 640, height: 360, x: 200, y: 360 };
const blade: Crop = { width: 640, height: 360, x: 940, y: 460 };
const tip: Crop = { width: 480, height: 270, x: 860, y: 560 };
const cameraClip = "240 (1).mp4";

const maxTexture = 4096;
const quality = ["output.quality=8"];
const lossless = ["output.encoder=software", "output.quality=0"];

export const sourceRate = 240;
export const recordings: Record<string, number> = {
	"240.mp4": 1014 / sourceRate,
	[cameraClip]: 1387 / sourceRate
};

export const moment = {
	file: "moment.avif",
	clip: cameraClip,
	start: 544 / sourceRate,
	crop: { width: 1280, height: 720, x: 176, y: 90 },
	frames: 65,
	columns: 6,
	width: 640,
	height: 360,
	center: 32,
	rate: 960,
	real: 4,
	playback: 60,
	settings: [...quality, "blending.enabled=no", "interpolation.fps=960", "dedup.enabled=no"]
};

export const stills = {
	file: "frames.webp",
	clip: cameraClip,
	seconds: recordings[cameraClip],
	at: 16 / 60,
	width: world.width
};

const each = (values: (string | number)[], settings: (value: string) => string[]) =>
	values.map((value): [string, string[]] => [`${value}`, settings(`${value}`)]);

const slow = "setpts=N/60/TB";
const between = (phase: number) => `select='eq(mod(n,4),${phase})',${slow}`;
const recorded: [string, null] = ["recorded", null];
const engines: [string, string[] | null][] = [
	recorded,
	["svp", []],
	["rife", ["interpolation.engine=rife"]]
];

const masked = {
	clip: cameraClip,
	crop: chat,
	zoom: 2,
	thin: between(0),
	quality: 0
};
const masking = {
	...masked,
	span: [0.4, 1.2],
	still: 22,
	filter: between(2),
	offset: 2 / 240,
	base: [...lossless, "blending.enabled=no", "interpolation.fps=240"]
};
const off = ["mask.enabled=no"];
const still = ["mask.method=still"];
export const chatMask = {
	name: "chat",
	width: 1920,
	height: 1080,
	box: { width: 250, height: 170, x: 0, y: 860 }
};
const image = ["mask.method=image", `mask.image=${chatMask.name}`];
const loose = ["mask.tolerance=100"];
const fine = ["mask.edge=0"];
const grain = { ...masking, crop: { width: 480, height: 270, x: 200, y: 300 }, still: 19 };

const repeats = {
	clip: cameraClip,
	crop: { width: 480, height: 270, x: 196, y: 200 },
	zoom: 2,
	span: [2, 2.6],
	still: 14,
	thin: "select='not(mod(n,4))',setpts=N/60/TB,fps=240:round=up",
	rate: 240
};

const pre = ["pre-interpolation.enabled=yes"];
const frames = {
	thin: between(0),
	base: ["blending.enabled=no", "interpolation.fps=240"],
	filter: between(2),
	offset: 2 / 240
};

const swing = { crop: blade, zoom: 2, span: [0.6, 1.2], still: 10 };
const edge = { ...swing, crop: { ...blade, x: 1140 }, still: 26 };
const close = { ...frames, ...swing, crop: tip, still: 22 };
const models = [
	"rife-v4.6",
	"rife-v4.15-lite",
	"rife-v4.22-lite",
	"rife-v4.25",
	"rife-v4.25-lite",
	"rife-v4.26"
];

const exact = ["interpolation.enabled=no", "blending.enabled=no", "dedup.enabled=no"];

const graded = {
	clip: cameraClip,
	crop: { width: 1280, height: 720, x: 0, y: 0 },
	filter: "scale=1280:720:flags=lanczos",
	span: [0.55, 1.15],
	still: 10,
	base: [...exact, ...lossless]
};
const grade = (key: string, values: number[]) =>
	each(values, (value) => ["color.enabled=yes", `color.${key}=${value}`]);

const upscale = ["upscale.enabled=yes"];
const upscaled = {
	crop: { width: 192, height: 108, x: 1890, y: 996 },
	zoom: 5,
	span: [0.6, 1.2],
	still: 11,
	base: [...exact, ...lossless]
};

const encoded = {
	crop: { width: 640, height: 360, x: 1060, y: 420 },
	zoom: 2,
	span: [0.6, 1.2],
	still: 10,
	base: ["output.encoder=software"]
};

const original: [string, null] = ["original", null];

export const grids = {
	clip: {
		clip: cameraClip,
		crop: wall,
		span: [1, 1.6],
		variants: [original, ["result", []]]
	},
	overview: {
		clip: cameraClip,
		crop: { width: 960, height: 540, x: 0, y: 0 },
		span: [1, 1.6],
		still: 16,
		filter: "scale=960:540:flags=lanczos",
		variants: [original, ["result", []]]
	},
	compare: {
		clip: cameraClip,
		crop: { width: 1280, height: 720, x: 0, y: 0 },
		span: [1, 1.6],
		filter: "scale=1280:720:flags=lanczos",
		variants: [
			["original", null],
			["result", []]
		]
	},
	masking: {
		...masking,
		variants: [
			["off", off],
			["peel", ["mask.method=peel"]],
			["still", still],
			["image", image]
		]
	},
	"soft-edge": {
		...masking,
		crop: { ...chat, y: 720 },
		variants: each([0, 100], (soft) => [...image, `mask.soft edge=${soft}`])
	},
	"peel-limit": {
		...masking,
		variants: [
			["6", []],
			["20", ["mask.peel limit=20"]]
		]
	},
	"still-limit": {
		...masked,
		span: [4.6, 5.2],
		still: 12,
		base: lossless,
		variants: [
			["4.5", still],
			["20", [...still, "mask.limit=20"]]
		]
	},
	"still-edge": {
		...grain,
		variants: [
			["0", [...still, ...loose, ...fine]],
			["5", [...still, ...loose]]
		]
	},
	"still-tolerance": {
		...grain,
		variants: [
			["30", [...still, ...fine]],
			["100", [...still, ...fine, ...loose]]
		]
	},
	repeats: {
		...repeats,
		variants: [
			["off", ["dedup.enabled=no"]],
			["on", []]
		]
	},
	"repeats-replaced": {
		...repeats,
		variants: [
			["0", ["dedup.repeats=0"]],
			["50", ["dedup.repeats=50"]],
			["100", []]
		]
	},
	pre: {
		...frames,
		...swing,
		crop: tip,
		still: 7,
		filter: between(1),
		offset: 1 / 240,
		variants: [
			recorded,
			["off", ["pre-interpolation.enabled=no"]],
			["2x", pre],
			["4x", [...pre, "pre-interpolation.fps=4x"]]
		]
	},
	models: {
		...frames,
		...swing,
		crop: { width: 480, height: 270, x: 1250, y: 470 },
		span: [0.6, 1.8],
		still: 26,
		base: [...frames.base, ...pre, "pre-interpolation.fps=4x"],
		variants: [recorded, ...each(models, (model) => [`rife.model=${model}`])]
	},
	backend: {
		...frames,
		...edge,
		zoom: 1,
		base: [...frames.base, ...pre, "pre-interpolation.fps=4x"],
		variants: each(["vulkan", "tensorrt"], (backend) => [`rife.backend=${backend}`])
	},
	engine: {
		...frames,
		...edge,
		variants: engines
	},
	artifacts: {
		...frames,
		...swing,
		crop: { width: 640, height: 360, x: 900, y: 620 },
		still: 11,
		variants: engines
	},
	fps: {
		clip: cameraClip,
		crop: { width: 640, height: 360, x: 250, y: 300 },
		zoom: 2,
		span: [2, 2.6],
		still: 18,
		thin: frames.thin,
		variants: each([120, 240, 480, 960], (rate) => [`interpolation.fps=${rate}`])
	},
	speed: {
		...close,
		variants: [
			recorded,
			["medium", ["interpolation.speed=medium"]],
			["fast", ["interpolation.speed=fast"]],
			["faster", []],
			["fastest", ["interpolation.speed=fastest"]]
		]
	},
	tuning: {
		...close,
		variants: [
			recorded,
			["weak", ["interpolation.tuning=weak"]],
			["film", []],
			["animation", ["interpolation.tuning=animation"]]
		]
	},
	look: {
		...close,
		variants: [recorded, ["sharp", ["interpolation.algorithm=2"]], ["standard", []]]
	},
	half: {
		...close,
		variants: [recorded, ["yes", []], ["no", ["interpolation.half size=no"]]]
	},
	"color-brightness": { ...graded, variants: grade("brightness", [0.5, 1, 1.5]) },
	"color-contrast": { ...graded, variants: grade("contrast", [0.5, 1, 1.5]) },
	"color-saturation": { ...graded, variants: grade("saturation", [0, 1, 2]) },
	"color-hue": { ...graded, variants: grade("hue", [-90, 0, 90]) },
	"color-lut": {
		...graded,
		variants: [
			["none", ["color.enabled=no"]],
			["look", ["color.enabled=yes", "color.lut=look.cube"]]
		]
	},
	"upscale-method": {
		...upscaled,
		variants: each(["nearest", "bilinear", "bicubic", "lanczos"], (method) => [
			...upscale,
			`upscale.method=${method}`
		])
	},
	"output-quality": {
		...encoded,
		variants: each([20, 30, 40], (quality) => [`output.quality=${quality}`])
	}
} satisfies Record<string, Grid>;

type Grid = {
	clip?: string;
	rate?: number;
	crop: Crop;
	zoom?: number;
	span: number[];
	still?: number;
	quality?: number;
	thin?: string;
	base?: string[];
	filter?: string;
	offset?: number;
	variants: [string, string[] | null][];
};

export type GridName = keyof typeof grids;
export type AssetName = "moment" | GridName;

export const gridOf = (name: GridName): Grid => grids[name];

export const variantNames = (grid: GridName) => gridOf(grid).variants.map(([variant]) => variant);

export const isStill = (grid: GridName) => "still" in gridOf(grid);

export function shapeOf(grid: GridName) {
	const { crop, zoom = 1 } = gridOf(grid);
	const tiles = variantNames(grid).length;
	const columns = Math.min(tiles, Math.floor(maxTexture / (crop.width * zoom)));
	return { columns, rows: Math.ceil(tiles / columns) };
}

export const renderOf = (grid: GridName, settings: string[]) => [
	...quality,
	...(gridOf(grid).base ?? []),
	...settings
];

export const rateOf = (grid: GridName) =>
	gridOf(grid).rate ?? (gridOf(grid).thin ? gridRate : sourceRate);

export const gridRate = 60;
