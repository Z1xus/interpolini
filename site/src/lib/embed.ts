import { Tokenizer } from "@huggingface/tokenizers";
import { InferenceSession, Tensor } from "onnxruntime-web/wasm";

const limit = 512;
const weights = "model_quantized.onnx_data";

export const query = (text: string) =>
	`Represent this sentence for searching relevant passages: ${text}`;

export async function embedder(read: (file: string) => Promise<ArrayBuffer>) {
	const json = async (file: string) => JSON.parse(new TextDecoder().decode(await read(file)));
	const [tokens, config, graph, data] = await Promise.all([
		json("tokenizer.json"),
		json("tokenizer_config.json"),
		read("onnx/model_quantized.onnx"),
		read(`onnx/${weights}`)
	]);
	const tokenizer = new Tokenizer(tokens, config);
	const session = await InferenceSession.create(new Uint8Array(graph), {
		externalData: [{ path: weights, data: new Uint8Array(data) }]
	});
	return async (text: string) => {
		const all = tokenizer.encode(text).ids;
		const ids = all.length > limit ? [...all.slice(0, limit - 1), all[all.length - 1]] : all;
		const tensor = (value: (id: number) => number) =>
			new Tensor(
				"int64",
				BigInt64Array.from(ids, (id) => BigInt(value(id))),
				[1, ids.length]
			);
		const { sentence_embedding } = await session.run({
			input_ids: tensor((id) => id),
			attention_mask: tensor(() => 1),
			token_type_ids: tensor(() => 0)
		});
		const vector = sentence_embedding.data as Float32Array;
		const length = Math.hypot(...vector);
		return vector.map((value) => value / length);
	};
}
