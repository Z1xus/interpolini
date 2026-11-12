import { goto } from "$app/navigation";

export const hashOf = (id: string, value?: string) =>
	value === undefined ? `#${id}` : `#${id}=${encodeURIComponent(value)}`;

const decoded = (text: string) => {
	try {
		return decodeURIComponent(text);
	} catch {
		return undefined;
	}
};

export function parseHash(hash: string) {
	const [, id, value] = /^#([^=]+)(?:=(.*))?$/.exec(hash) ?? [];
	return { id, value: value === undefined ? undefined : decoded(value) };
}

export const writeHash = (id: string, value?: string) =>
	goto(hashOf(id, value), { shallow: true, replace: true, reset: false });
