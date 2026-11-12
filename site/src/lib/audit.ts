import { building } from "$app/env";
import { pages, route } from "#lib/pages.ts";

const rendered = new Set<string>();

export const record = (id: string) => {
	if (building) rendered.add(id);
};

export function audit(routeId: string | null) {
	const page = pages.find((entry) => route(entry) === routeId);
	const found = [...rendered];
	rendered.clear();
	if (!building || !page) return;
	const listed = page.sections.map(({ id }) => id as string);
	const missing = listed.filter((id) => !found.includes(id));
	const unlisted = found.filter((id) => !listed.includes(id));
	if (missing.length > 0 || unlisted.length > 0) {
		throw new Error(
			`${routeId}: sections in pages.ts but not on the page: [${missing}], on the page but not in pages.ts: [${unlisted}]`
		);
	}
}
