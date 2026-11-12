import type { Handle } from "@sveltejs/kit/hooks";
import { audit } from "#lib/audit.ts";

export const handle: Handle = async ({ event, resolve }) => {
	const response = await resolve(event);
	audit(event.route.id);
	return response;
};
