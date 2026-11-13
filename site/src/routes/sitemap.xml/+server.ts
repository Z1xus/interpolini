import { resolve } from "$app/paths";
import { origin, pages, route } from "#lib/pages.ts";

export const prerender = true;

export const GET = () => {
	const urls = pages.map((page) => {
		const path = resolve(route(page));
		return `<url><loc>${new URL(path.endsWith("/") ? path : `${path}/`, origin)}</loc></url>`;
	});
	return new Response(
		`<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">${urls.join("")}</urlset>`,
		{ headers: { "Content-Type": "application/xml" } }
	);
};
