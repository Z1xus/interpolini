import { cpSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { embedder } from "../src/lib/embed.ts";
import type { Entry } from "../src/lib/search.ts";

const revision = "4262131b32c3182bd06e67e92ae69d7bd66e0c5c";
const hub = `https://huggingface.co/MongoDB/mdbr-leaf-ir/resolve/${revision}`;
const files = [
	"tokenizer.json",
	"tokenizer_config.json",
	"onnx/model_quantized.onnx",
	"onnx/model_quantized.onnx_data"
];
const site = join(import.meta.dir, "..");
const model = join(site, "static/model");
const build = join(site, "build");
const outputs = [build, join(site, ".svelte-kit/output/client")];

if (!files.every((file) => existsSync(join(model, file)))) {
	for (const file of files) {
		const response = await fetch(`${hub}/${file}`);
		if (!response.ok) throw new Error(`${file}: ${response.status}`);
		mkdirSync(dirname(join(model, file)), { recursive: true });
		await Bun.write(join(model, file), await response.arrayBuffer());
	}
	for (const output of outputs) cpSync(model, join(output, "model"), { recursive: true });
}

const asides = ["dl", "table", "pre", "fieldset", "label"];
const clean = (text: string) => text.replace(/\s+/g, " ").trim();

async function read(file: string) {
	const folder = relative(build, dirname(file));
	const path = folder && `${folder}/`;
	const entries: Omit<Entry, "vector" | "group">[] = [];
	let heading = "";
	let group = "";
	let page = "";
	let description = "";
	let skip = 0;
	let aside = 0;
	const last = () => entries[entries.length - 1];
	const within = (change: (by: number) => void) => (element: HTMLRewriterTypes.Element) => {
		change(1);
		element.onEndTag(() => change(-1));
	};
	const rewriter = new HTMLRewriter()
		.on('meta[name="description"]', {
			element: (element) => void (description = element.getAttribute("content") ?? "")
		})
		.on("#menu h2", {
			element: () => void (heading = ""),
			text: ({ text }) => void (heading += text)
		})
		.on('#menu a[aria-current="page"]', { element: () => void (group = clean(heading)) })
		.on("article h1", { text: ({ text }) => void (page += text) })
		.on("article section", {
			element: (element) =>
				void entries.push({
					page,
					title: "",
					text: "",
					extra: "",
					path: `${path}#${element.getAttribute("id")}`
				})
		})
		.on("article section h2", { text: ({ text }) => void (last().title += text) })
		.on("article section h2, article section button, article section span.sr-only", {
			element: within((by) => (skip += by))
		})
		.on(asides.map((tag) => `article section ${tag}`).join(", "), {
			element: within((by) => (aside += by))
		})
		.on("article section *", {
			element() {
				last().text += " ";
				last().extra += " ";
			},
			text({ text }) {
				if (!skip) last()[aside ? "extra" : "text"] += text;
			}
		});
	await rewriter.transform(new Response(Bun.file(file))).text();
	const top = { page, title: page, text: description, extra: "", path };
	return [top, ...entries].map((entry) => ({
		...entry,
		group,
		text: clean(entry.text),
		extra: clean(entry.extra)
	}));
}

const pages = [...new Bun.Glob("**/index.html").scanSync(build)].sort();
const entries = (await Promise.all(pages.map((page) => read(join(build, page))))).flat();
const embed = await embedder((file) => Bun.file(join(model, file)).arrayBuffer());
const index: Entry[] = [];
for (const entry of entries) {
	const { group, page, title, text, extra } = entry;
	const vector = await embed(`${group}, ${page}: ${title}. ${text} ${extra}`);
	const bytes = Int8Array.from(vector, (value) => Math.round(value * 127));
	index.push({ ...entry, vector: Buffer.from(bytes.buffer).toString("base64") });
}
for (const output of outputs) await Bun.write(join(output, "search.json"), JSON.stringify(index));
console.log(`search: ${index.length} entries`);
