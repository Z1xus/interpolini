import MiniSearch from "minisearch";

export type Entry = {
	group: string;
	page: string;
	title: string;
	text: string;
	extra: string;
	path: string;
	vector: string;
};

const count = 8;
const share = 0.5;

export function searcher(entries: readonly Entry[]) {
	const vectors = entries.map(({ vector }) =>
		Int8Array.from(atob(vector), (character) => character.charCodeAt(0))
	);
	const words = new MiniSearch({
		fields: ["title", "page", "text", "extra"],
		searchOptions: { boost: { title: 3, page: 1.5 }, prefix: true, fuzzy: 0.2, combineWith: "AND" }
	});
	words.addAll(entries.map((entry, id) => ({ id, ...entry })));

	return (text: string, meaning?: Float32Array) => {
		const scores = new Float32Array(entries.length);
		const strict = words.search(text);
		const found = strict.length || meaning ? strict : words.search(text, { combineWith: "OR" });
		for (const { id, score } of found) scores[id] = (share * score) / found[0].score;
		if (meaning) {
			const near = vectors.map((vector) =>
				vector.reduce((sum, value, at) => sum + value * meaning[at], 0)
			);
			const top = Math.max(...near);
			const mean = near.reduce((sum, value) => sum + value) / near.length;
			near.forEach((value, id) => {
				scores[id] += (1 - share) * Math.max((value - mean) / (top - mean), 0);
			});
		}
		return [...scores.keys()]
			.filter((id) => scores[id] > 0)
			.sort((a, b) => scores[b] - scores[a])
			.slice(0, count)
			.map((id) => entries[id]);
	};
}
