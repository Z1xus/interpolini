import { includeIgnoreFile } from "@eslint/compat";
import js from "@eslint/js";
import prettier from "eslint-config-prettier";
import svelte from "eslint-plugin-svelte";
import { defineConfig } from "eslint/config";
import globals from "globals";
import { fileURLToPath } from "node:url";
import ts from "typescript-eslint";

const site = {
	rules: {
		"no-comments": {
			meta: { type: "suggestion", messages: { comment: "Code explains itself. No comments." } },
			create: (context) => ({
				Program() {
					for (const comment of context.sourceCode.getAllComments()) {
						context.report({ loc: comment.loc, messageId: "comment" });
					}
				}
			})
		}
	}
};

export default defineConfig(
	includeIgnoreFile(fileURLToPath(new URL(".gitignore", import.meta.url))),
	{ ignores: ["static"] },
	js.configs.recommended,
	ts.configs.strict,
	svelte.configs.recommended,
	prettier,
	svelte.configs.prettier,
	{
		plugins: { site },
		languageOptions: { globals: { ...globals.browser, ...globals.node } },
		rules: {
			"site/no-comments": "error",
			"no-console": ["error", { allow: ["error"] }],
			"no-undef": "off",
			eqeqeq: "error",
			"object-shorthand": "error",
			"prefer-template": "error",
			"no-useless-rename": "error",
			"no-useless-concat": "error",
			"no-useless-return": "error",
			"no-unneeded-ternary": "error",
			"no-else-return": "error",
			"no-lonely-if": "error",
			"no-implicit-coercion": "error",
			"svelte/no-navigation-without-resolve": "off"
		}
	},
	{
		files: ["**/*.svelte", "**/*.svelte.ts"],
		languageOptions: {
			parserOptions: { projectService: true, extraFileExtensions: [".svelte"], parser: ts.parser }
		}
	},
	{
		files: ["scripts/**"],
		rules: { "no-console": "off" }
	}
);
