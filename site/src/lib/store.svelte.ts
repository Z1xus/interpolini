import { SvelteMap } from "svelte/reactivity";
import type { AssetName } from "#lib/assets.ts";
import { load, release, type Resource } from "#lib/loader.ts";

type Entry =
	| { status: "loading"; controller: AbortController }
	| { status: "ready"; resource: Resource }
	| { status: "failed" };

export class Store {
	#entries = new SvelteMap<AssetName, Entry>();
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
		for (const name of [...this.#entries.keys()]) {
			if (!names.includes(name)) this.#drop(name);
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
	}
}
