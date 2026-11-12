import { exists, mkdir, rm, symlink } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import {
	chatMask,
	gridOf,
	gridRate,
	grids,
	moment,
	recordings,
	rateOf,
	renderOf,
	shapeOf,
	stills,
	type GridName
} from "#lib/assets.ts";

const cli =
	process.env.CLI ??
	join(homedir(), "dev/interpolini/dist/interpolini-linux-x86_64/interpolini-cli");
const docs = resolve(process.env.DOCS_OUT ?? join(import.meta.dir, "../.publish/docs"));
const output = join(docs, "media");
const work = join(output, ".work");
const limit = [
	"systemd-run",
	"--user",
	"--scope",
	"-q",
	"-p",
	"MemoryMax=8G",
	"-p",
	"MemorySwapMax=0"
];
const quiet = ["ffmpeg", "-y", "-v", "error"];
const sharpQuality = 24;
const stillQuality = 16;
const stillColor =
	"scale=in_range=tv:in_color_matrix=bt709:out_range=pc:out_color_matrix=bt709,format=yuv444p";
const stillEncoder = (quality: number) => [
	...["-frames:v", "1", "-c:v", "libaom-av1", "-still-picture", "1", "-crf", `${quality}`],
	...["-b:v", "0", "-cpu-used", "4", "-color_primaries", "bt709", "-color_trc", "iec61966-2-1"],
	...["-colorspace", "bt709", "-color_range", "pc"]
];

const [input, ...only] = Bun.argv.slice(2);
if (!input) {
	throw new Error("usage: bun scripts/media.ts <source clip> [grid...]");
}
const source = resolve(input);

async function run(command: string[]) {
	const process = Bun.spawn(command, { cwd: work, stdout: "inherit", stderr: "inherit" });
	if ((await process.exited) !== 0) {
		throw new Error(`${command[0]} failed`);
	}
}

const crop = ({ width, height, x, y }: typeof moment.crop) => `crop=${width}:${height}:${x}:${y}`;

async function render(clip: string, name: string, settings: string[]) {
	const folder = join(work, name);
	await mkdir(folder);
	await symlink(clip, join(folder, "clip.mp4"));
	await run([
		...limit,
		cli,
		...["-c", join(work, "base.ini")],
		...settings.flatMap((s) => ["-s", s]),
		join(folder, "clip.mp4")
	]);
	return join(folder, "clip - interpolini.mp4");
}

const wanted = (name: string) => only.length === 0 || only.includes(name);

if (only.length === 0) await rm(output, { recursive: true, force: true });
await rm(work, { recursive: true, force: true });
await mkdir(work, { recursive: true });
await Bun.write(join(work, "base.ini"), "");

const lutSteps = 17;
const shadows = [-0.1, 0.02, 0.12];
const highlights = [0.15, 0.05, -0.12];

function lut() {
	const nodes: string[] = [];
	for (let blue = 0; blue < lutSteps; blue++) {
		for (let green = 0; green < lutSteps; green++) {
			for (let red = 0; red < lutSteps; red++) {
				const rgb = [red, green, blue].map((step) => step / (lutSteps - 1));
				const luma = (rgb[0] + rgb[1] + rgb[2]) / 3;
				const graded = rgb.map((value, index) =>
					Math.min(1, Math.max(0, value + shadows[index] * (1 - luma) + highlights[index] * luma))
				);
				nodes.push(graded.map((value) => value.toFixed(6)).join(" "));
			}
		}
	}
	return `LUT_3D_SIZE ${lutSteps}\n${nodes.join("\n")}\n`;
}

await Bun.write(join(work, "look.cube"), lut());

const masks = join(dirname(cli), "masks");
const maskFile = join(masks, `${chatMask.name}.png`);
const masked = wanted("masking") || wanted("soft-edge");
const madeMasks = !(await exists(masks));
if (masked) {
	if (await exists(maskFile)) throw new Error(`${maskFile} is in the way`);
	const { width, height, x, y } = chatMask.box;
	await mkdir(masks, { recursive: true });
	await run([
		...quiet,
		...["-f", "lavfi", "-i", `color=white:s=${chatMask.width}x${chatMask.height}`],
		...["-vf", `drawbox=x=${x}:y=${y}:w=${width}:h=${height}:color=black:t=fill,format=gray`],
		...["-frames:v", "1", maskFile]
	]);
}

try {
	for (const name of Object.keys(grids) as GridName[]) {
		if (!wanted(name)) continue;
		const grid = gridOf(name);
		const [from, to] = grid.span;
		const filter = grid.filter ? `${grid.filter},` : "";
		const clip = grid.clip ? resolve(dirname(source), grid.clip) : source;
		const trimmed = join(work, `${name}.mp4`);
		await run([
			...quiet,
			...["-ss", `${from}`, "-to", `${to}`, "-i", clip, "-an", "-fps_mode", "passthrough"],
			...["-c:v", "libx264", "-crf", "10", "-preset", "veryfast", trimmed]
		]);
		let thinned = trimmed;
		if (grid.thin) {
			thinned = join(work, `${name}-thin.mp4`);
			await run([
				...quiet,
				...["-i", trimmed, "-vf", grid.thin, "-r", `${rateOf(name)}`],
				...["-c:v", "libx264", "-crf", "10", "-preset", "veryfast", thinned]
			]);
		}
		const inputs: string[] = [];
		for (const [variant, settings] of grid.variants) {
			inputs.push(
				settings ? await render(thinned, `${name}-${variant}`, renderOf(name, settings)) : trimmed
			);
		}
		const still = grid.still !== undefined;
		const picked = still ? `select=eq(n\\,${grid.still}),` : "";
		const converted = still ? `,${stillColor}` : "";
		const zoom = grid.zoom ?? 1;
		const zoomed = zoom > 1 ? `,scale=iw*${zoom}:ih*${zoom}:flags=neighbor` : "";
		const tiles = inputs.map(
			(_, index) =>
				`[${index}:v]${filter}fps=${gridRate}:round=up,${picked}${crop(grid.crop)}${zoomed}${converted}[v${index}]`
		);
		const { columns } = shapeOf(name);
		const [width, height] = [grid.crop.width * zoom, grid.crop.height * zoom];
		const layout = inputs.map(
			(_, index) => `${(index % columns) * width}_${Math.floor(index / columns) * height}`
		);
		const stacked = `${inputs.map((_, index) => `[v${index}]`).join("")}xstack=inputs=${inputs.length}:layout=${layout.join("|")}`;
		const encode = still
			? [...stillEncoder(grid.quality ?? stillQuality), join(output, `${name}.avif`)]
			: [
					...["-an", "-c:v", "libvpx-vp9", "-crf", `${grid.quality ?? sharpQuality}`],
					...["-b:v", "0", "-row-mt", "1", "-cpu-used", "1", "-pix_fmt", "yuv420p"],
					join(output, `${name}.webm`)
				];
		await run([
			...quiet,
			...inputs.flatMap((input) => ["-i", input]),
			...["-filter_complex", `${tiles.join(";")};${stacked}`],
			...encode
		]);
	}
} finally {
	if (masked) await rm(madeMasks ? masks : maskFile, { recursive: true, force: true });
}

if (wanted("compare")) {
	await run([
		...quiet,
		...["-ss", `${stills.at}`, "-i", join(output, "compare.webm")],
		...["-frames:v", "1", "-c:v", "libwebp", "-quality", "95", join(output, stills.file)]
	]);
	await rm(join(output, "compare.webm"));
}

async function capture(command: string[]) {
	const process = Bun.spawn(command, { stdout: "pipe", stderr: "inherit" });
	const bytes = await new Response(process.stdout).arrayBuffer();
	if ((await process.exited) !== 0) {
		throw new Error(`${command[0]} failed`);
	}
	return bytes;
}

if (wanted("thumbs")) {
	const clip = resolve(dirname(source), stills.clip);
	const thumbWidth = 160;
	const thumbHeight = 90;
	const thumbCount = 8;
	const [seconds, peaks] = [stills.seconds, 120];
	for (const [input, duration, file] of [
		[clip, seconds, "thumbs.webp"],
		[source, recordings["240.mp4"], "thumbs-sword.webp"]
	] as const) {
		await run([
			...quiet,
			...["-i", input],
			...[
				"-vf",
				`fps=${thumbCount}/${duration},scale=${thumbWidth}:${thumbHeight},tile=1x${thumbCount}`
			],
			...["-frames:v", "1", "-c:v", "libwebp", "-quality", "85", join(output, file)]
		]);
	}
	const audioStream = await capture([
		...["ffprobe", "-v", "error", "-select_streams", "a:0"],
		...["-show_entries", "stream=index", "-of", "csv=p=0", clip]
	]);
	const samples =
		audioStream.byteLength > 0
			? new Int16Array(
					await capture([
						...quiet,
						...["-i", clip, "-map", "0:a:0", "-ac", "1", "-ar", "4000", "-f", "s16le", "-"]
					])
				)
			: new Int16Array(peaks);
	const bucket = Math.ceil(samples.length / peaks);
	await Bun.write(
		join(output, "wave.bin"),
		Uint8Array.from({ length: peaks }, (_, index) => {
			const part = samples.subarray(index * bucket, (index + 1) * bucket);
			return Math.round(
				(part.reduce((peak, value) => Math.max(peak, Math.abs(value)), 0) / 32768) * 255
			);
		})
	);
}

if (wanted("moment")) {
	const stack = join(work, "moment.mp4");
	await run([
		...quiet,
		...[
			"-ss",
			`${moment.start}`,
			"-i",
			resolve(dirname(source), moment.clip),
			"-frames:v",
			`${(moment.frames - 1) / moment.real + 1}`
		],
		...["-an", "-c:v", "libx264", "-crf", "8", "-preset", "veryfast", stack]
	]);
	const frames = await render(stack, "moment", moment.settings);
	const rows = Math.ceil(moment.frames / moment.columns);
	await run([
		...quiet,
		...["-i", frames, "-frames:v", `${moment.frames}`],
		...[
			"-vf",
			`${crop(moment.crop)},scale=${moment.width}:${moment.height}:flags=lanczos,tile=${moment.columns}x${rows}`
		],
		...["-frames:v", "1", "-c:v", "libaom-av1", "-still-picture", "1", "-crf", "20", "-b:v", "0"],
		...["-cpu-used", "6", "-pix_fmt", "yuv420p", join(output, moment.file)]
	]);
}

await rm(work, { recursive: true, force: true });
