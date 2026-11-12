import { moment } from "#lib/assets.ts";
import frames from "#lib/shaders/frames.glsl?raw";
import common from "#lib/shaders/common.glsl?raw";
import tiles from "#lib/shaders/tiles.glsl?raw";
import vertex from "#lib/shaders/vertex.glsl?raw";
import type { Resource } from "#lib/loader.ts";
import { required } from "#lib/required.ts";
import type { Blend, View } from "#lib/show.ts";

const views: Record<View, number> = { normal: 0, difference: 1 };
const rows = Math.ceil(moment.frames / moment.columns);

type Sheet = { width: number; height: number; columns: number; rows: number };

function compile(gl: WebGL2RenderingContext, type: number, source: string) {
	const shader = required(gl.createShader(type));
	gl.shaderSource(shader, source);
	gl.compileShader(shader);
	if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
		throw new Error(gl.getShaderInfoLog(shader) ?? "shader failed");
	}
	return shader;
}

function program<N extends string>(gl: WebGL2RenderingContext, stage: string, names: readonly N[]) {
	const shaders = [
		compile(gl, gl.VERTEX_SHADER, vertex),
		compile(gl, gl.FRAGMENT_SHADER, common + stage)
	];
	const linked = required(gl.createProgram());
	for (const shader of shaders) gl.attachShader(linked, shader);
	gl.linkProgram(linked);
	if (!gl.getProgramParameter(linked, gl.LINK_STATUS)) {
		throw new Error(gl.getProgramInfoLog(linked) ?? "program failed");
	}
	for (const shader of shaders) {
		gl.detachShader(linked, shader);
		gl.deleteShader(shader);
	}
	const uniforms = Object.fromEntries(
		names.map((name) => [name, gl.getUniformLocation(linked, name)])
	) as Record<N, WebGLUniformLocation | null>;
	return { linked, uniforms };
}

function createTexture(gl: WebGL2RenderingContext, unit: number, filter: number) {
	const created = required(gl.createTexture());
	gl.activeTexture(gl.TEXTURE0 + unit);
	gl.bindTexture(gl.TEXTURE_2D, created);
	gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, filter);
	gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, filter);
	gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
	gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
	return created;
}

export function createRenderer(canvas: HTMLCanvasElement) {
	const gl = required(canvas.getContext("webgl2", { alpha: false }));
	const tilesProgram = program(gl, tiles, ["view", "video", "grid", "tile"]);
	const framesProgram = program(gl, frames, [
		"view",
		"atlas",
		"weights",
		"grid",
		"frames",
		"count",
		"center",
		"stride",
		"gamma"
	]);
	const textures = [
		createTexture(gl, 0, gl.LINEAR),
		createTexture(gl, 1, gl.LINEAR),
		createTexture(gl, 2, gl.NEAREST)
	];
	let atlas: ImageBitmap | undefined;
	let weights: Float32Array | undefined;

	const resize = (width: number, height: number) => {
		if (canvas.width !== width) canvas.width = width;
		if (canvas.height !== height) canvas.height = height;
		gl.viewport(0, 0, width, height);
	};

	const draw = (active: WebGLProgram, view: View, uniform: WebGLUniformLocation | null) => {
		gl.useProgram(active);
		gl.uniform1i(uniform, views[view]);
		gl.drawArrays(gl.TRIANGLES, 0, 3);
	};

	return {
		tiles(source: Resource, sheet: Sheet, index: number, view: View) {
			const { linked, uniforms } = tilesProgram;
			resize(sheet.width / sheet.columns, sheet.height / sheet.rows);
			gl.useProgram(linked);
			gl.activeTexture(gl.TEXTURE0);
			gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, source);
			gl.uniform1i(uniforms.video, 0);
			gl.uniform2i(uniforms.grid, sheet.columns, sheet.rows);
			gl.uniform1i(uniforms.tile, index);
			draw(linked, view, uniforms.view);
		},
		frames(source: ImageBitmap, plan: Float32Array, blend: Blend, frame: number, view: View) {
			const { linked, uniforms } = framesProgram;
			resize(moment.width, moment.height);
			gl.useProgram(linked);
			if (source !== atlas) {
				gl.activeTexture(gl.TEXTURE1);
				gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, source);
				atlas = source;
			}
			if (plan !== weights) {
				gl.activeTexture(gl.TEXTURE2);
				gl.texImage2D(gl.TEXTURE_2D, 0, gl.R32F, plan.length, 1, 0, gl.RED, gl.FLOAT, plan);
				weights = plan;
			}
			gl.uniform1i(uniforms.atlas, 1);
			gl.uniform1i(uniforms.weights, 2);
			gl.uniform2i(uniforms.grid, moment.columns, rows);
			gl.uniform1i(uniforms.frames, moment.frames);
			gl.uniform1i(uniforms.count, plan.length);
			gl.uniform1i(uniforms.center, frame);
			gl.uniform1i(uniforms.stride, blend.stride);
			gl.uniform1f(uniforms.gamma, blend.gamma);
			draw(linked, view, uniforms.view);
		},
		dispose() {
			for (const texture of textures) gl.deleteTexture(texture);
			gl.deleteProgram(tilesProgram.linked);
			gl.deleteProgram(framesProgram.linked);
			gl.getExtension("WEBGL_lose_context")?.loseContext();
		}
	};
}

export type Renderer = ReturnType<typeof createRenderer>;
