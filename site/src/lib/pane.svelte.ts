import { getContext, setContext } from "svelte";
import type { Attachment } from "svelte/attachments";
import type { AssetName } from "#lib/assets.ts";
import type { Focus } from "#lib/focus.ts";
import { Playback } from "#lib/playback.svelte.ts";
import { Store } from "#lib/store.svelte.ts";
import { parseHash } from "#lib/url.ts";

type Registration = {
	id: string;
	assets: () => AssetName[];
	frame: () => number;
	select: (value?: string) => void;
};

type Entry = Registration & { element: HTMLElement };

const readingShare = 0.35;
const notch = 130;

class Pane {
	focus = $state<Focus>();
	active = $state<string>();
	playback = new Playback();
	store = new Store();
	#entries: Entry[] = [];
	#current = -1;
	#pinned = false;
	#starts: number[] = [];
	#dirty = false;
	#request = 0;

	reset() {
		this.active = undefined;
		this.#current = -1;
		this.#pinned = false;
		this.playback.stop();
	}

	activate(id: string) {
		const index = this.#entries.findIndex((entry) => entry.id === id);
		if (index < 0) return;
		this.#pinned = true;
		this.#enter(index);
	}

	restore(hash: string) {
		const { id, value } = parseHash(hash);
		const entry = this.#entries.find((candidate) => candidate.id === id);
		if (!entry) return;
		entry.select(value);
		entry.element.scrollIntoView();
		this.activate(id);
	}

	async warm(link: HTMLAnchorElement) {
		const text = await (await fetch(link.pathname)).text();
		const page = new DOMParser().parseFromString(text, "text/html");
		const section =
			page.getElementById(parseHash(link.hash).id) ?? page.querySelector("article section");
		const names = section?.getAttribute("data-media")?.split(" ").filter(Boolean) ?? [];
		this.store.warm(names as AssetName[]);
	}

	track(registration: Registration): Attachment<HTMLElement> {
		return (element) => {
			const entry: Entry = { ...registration, element };
			this.#entries.push(entry);
			this.#entries.sort((a, b) =>
				a.element.compareDocumentPosition(b.element) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1
			);
			this.#invalidate();
			return () => {
				this.#entries.splice(this.#entries.indexOf(entry), 1);
				this.#current = -1;
				this.#invalidate();
			};
		};
	}

	watch: Attachment<HTMLElement> = () => {
		const observer = new ResizeObserver(this.#invalidate);
		observer.observe(document.body);
		const unpin = () => (this.#pinned = false);
		addEventListener("scroll", this.#schedule, { passive: true });
		addEventListener("resize", this.#invalidate);
		for (const type of ["wheel", "touchmove", "keydown", "pointerdown"]) {
			addEventListener(type, unpin, { passive: true });
		}
		return () => {
			observer.disconnect();
			cancelAnimationFrame(this.#request);
			this.#request = 0;
			removeEventListener("scroll", this.#schedule);
			removeEventListener("resize", this.#invalidate);
			for (const type of ["wheel", "touchmove", "keydown", "pointerdown"]) {
				removeEventListener(type, unpin);
			}
		};
	};

	#invalidate = () => {
		this.#dirty = true;
		this.#schedule();
	};

	#schedule = () => {
		if (!this.#request) this.#request = requestAnimationFrame(this.#tick);
	};

	#tick = () => {
		this.#request = 0;
		if (this.#dirty) this.#measure();
		if (this.#pinned || this.#entries.length === 0) return;
		const at = Math.max(scrollY, 0);
		this.#enter(this.#starts.findLastIndex((start) => start <= at));
	};

	#measure() {
		this.#dirty = false;
		const top = parseFloat(getComputedStyle(document.documentElement).scrollPaddingTop) || 0;
		const line = scrollY - top - readingShare * (innerHeight - top);
		const reach = Math.max(document.documentElement.scrollHeight - innerHeight - 1, 1);
		const rects = this.#entries.map(({ element }) => element.getBoundingClientRect());
		const step = Math.min(notch, reach / Math.max(rects.length - 1, 1));
		const starts = rects.map((rect, index) =>
			index ? (rects[index - 1].bottom + rect.top) / 2 + line : 0
		);
		for (let index = 1; index < starts.length; index++) {
			starts[index] = Math.max(starts[index], starts[index - 1] + step);
		}
		for (let index = starts.length - 1, end = reach; index > 0; index--, end -= step) {
			starts[index] = Math.min(starts[index], end);
		}
		this.#starts = starts;
	}

	#enter(index: number) {
		if (index === this.#current) return;
		this.#current = index;
		const entry = this.#entries[index];
		this.active = entry.id;
		this.playback.stop();
		this.playback.frame = entry.frame();
		const near = this.#entries.slice(Math.max(index - 1, 0), index + 2);
		this.store.keep([...new Set(near.flatMap((entry) => entry.assets()))]);
	}
}

const key = Symbol("pane");

export const providePane = () => setContext(key, new Pane());
export const usePane = () => getContext<Pane>(key);
