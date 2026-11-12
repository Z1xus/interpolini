import { env } from "onnxruntime-web/wasm";
import wasm from "onnxruntime-web/ort-wasm-simd-threaded.wasm?url";
import { embedder, query } from "#lib/embed.ts";

env.wasm.wasmPaths = { wasm };
env.wasm.numThreads = 1;

let embed: ReturnType<typeof embedder>;

const failed = (error: unknown) => {
	console.error(error);
	postMessage({ ready: false });
};

onmessage = ({ data }: MessageEvent<{ root: string } | { text: string }>) => {
	if ("root" in data) {
		embed = embedder(async (file) => {
			const response = await fetch(data.root + file);
			if (!response.ok) throw new Error(`${file}: ${response.status}`);
			return response.arrayBuffer();
		});
		embed.then(() => postMessage({ ready: true }), failed);
		return;
	}
	embed
		.then((run) => run(query(data.text)))
		.then((vector) => postMessage({ text: data.text, vector }), failed);
};
