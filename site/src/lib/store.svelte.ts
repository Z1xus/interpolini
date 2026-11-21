import { SvelteMap } from "svelte/reactivity";
import type { AssetName } from "#lib/assets.ts";
import { load, release, type Resource } from "#lib/loader.ts";

type Entry =
	| { status: "loading"; controller: AbortController }
	| { status: "ready"; resource: Resource }
	| { status: "failed" };

const maxCachedBytes = 96 * 1024 * 1024;

export class Store {
	#entries = new SvelteMap<AssetName, Entry>();
	#recent: AssetName[] = [];
	#near: readonly AssetName[] = [];
	#ahead: readonly AssetName[] = [];

	get(name: AssetName) {
		return this.#entries.get(name);
	}

	keep(names: readonly AssetName[]) {
		this.#near = names;
		this.#sync();
	}

	warm(names: readonly AssetName[]) {
		this.#ahead = names;
		this.#sync();
	}

	#sync() {
		const names = [...this.#near, ...this.#ahead];
		for (const name of names) {
			const at = this.#recent.indexOf(name);
			if (at >= 0) this.#recent.splice(at, 1);
			this.#recent.push(name);
		}
		for (const name of [...this.#entries.keys()]) {
			if (names.includes(name)) continue;
			const entry = this.#entries.get(name);
			if (entry?.status === "ready" && entry.resource instanceof ImageBitmap) continue;
			this.#drop(name);
		}
		let cached = 0;
		const unused: { name: AssetName; bytes: number }[] = [];
		for (const name of this.#recent) {
			if (names.includes(name)) continue;
			const entry = this.#entries.get(name);
			if (entry?.status === "ready" && entry.resource instanceof ImageBitmap) {
				const bytes = entry.resource.width * entry.resource.height * 4;
				cached += bytes;
				unused.push({ name, bytes });
			}
		}
		for (const item of unused) {
			if (cached <= maxCachedBytes) break;
			cached -= item.bytes;
			this.#drop(item.name);
		}
		for (const name of names) {
			if (!this.#entries.has(name)) this.#start(name);
		}
	}

	retry(name: AssetName) {
		this.#drop(name);
		this.#start(name);
	}

	fail(name: AssetName) {
		if (this.#entries.get(name)?.status !== "ready") return;
		this.#drop(name);
		this.#entries.set(name, { status: "failed" });
	}

	#start(name: AssetName) {
		const controller = new AbortController();
		this.#entries.set(name, { status: "loading", controller });
		load(name, controller.signal, () => this.fail(name))
			.then((resource) => {
				if (controller.signal.aborted) release(resource);
				else this.#entries.set(name, { status: "ready", resource });
			})
			.catch((error) => {
				if (controller.signal.aborted) return;
				console.error(error);
				this.#entries.set(name, { status: "failed" });
			});
	}

	#drop(name: AssetName) {
		const entry = this.#entries.get(name);
		if (entry?.status === "loading") entry.controller.abort();
		if (entry?.status === "ready") release(entry.resource);
		this.#entries.delete(name);
		const at = this.#recent.indexOf(name);
		if (at >= 0) this.#recent.splice(at, 1);
	}
}
