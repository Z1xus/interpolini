import adapter from "@sveltejs/adapter-static";
import { sveltekit } from "@sveltejs/kit/vite";
import tailwindcss from "@tailwindcss/vite";
import { execFileSync } from "node:child_process";
import { defineConfig } from "vite";

const commit =
	process.env.GITHUB_SHA ?? execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();

export default defineConfig(({ mode }) => ({
	define: { COMMIT: JSON.stringify(commit) },
	plugins: [
		tailwindcss(),
		sveltekit({
			adapter: adapter(),
			paths: { base: mode === "production" ? "/interpolini" : "", relative: false }
		})
	]
}));
