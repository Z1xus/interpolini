import { join } from "node:path";

const site = join(import.meta.dir, "..");
const build = join(site, "build");
const outputs = [build, join(site, ".svelte-kit/output/client")];

const entities: Record<string, string> = {
	amp: "&",
	lt: "<",
	gt: ">",
	quot: '"',
	apos: "'",
	nbsp: " "
};

const decode = (text: string) =>
	text.replace(/&(#x[\da-f]+|#\d+|\w+);/gi, (match, code: string) => {
		if (code[0] === "#") {
			const radix = code[1]?.toLowerCase() === "x" ? 16 : 10;
			return String.fromCodePoint(parseInt(code.slice(radix === 16 ? 2 : 1), radix));
		}
		return entities[code.toLowerCase()] ?? match;
	});

const clean = (text: string) => text.replace(/\s+/g, " ").trim();

const codeSpan = (text: string) => {
	const fence = text.includes("`") ? "``" : "`";
	const pad = text.startsWith("`") || text.endsWith("`") ? " " : "";
	return `${fence}${pad}${text}${pad}${fence}`;
};

type Group = { title: string; slugs: string[] };
type Page = { slug: string; title: string; description: string; markdown: string };

async function readMenu() {
	let siteUrl = "";
	let basePath = "/";
	const groups: Group[] = [];
	const current = () => groups[groups.length - 1];

	const rewriter = new HTMLRewriter()
		.on('meta[property="og:url"]', {
			element(el) {
				const url = el.getAttribute("content");
				if (!url) return;
				siteUrl = url.endsWith("/") ? url : `${url}/`;
				basePath = new URL(siteUrl).pathname;
			}
		})
		.on("#menu section", {
			element() {
				groups.push({ title: "", slugs: [] });
			}
		})
		.on("#menu h2", {
			text({ text }) {
				current().title += text;
			}
		})
		.on("#menu a", {
			element(el) {
				const href = el.getAttribute("href");
				if (!href || !siteUrl) return;
				const pathname = new URL(decode(href), siteUrl).pathname;
				current().slugs.push(pathname.slice(basePath.length).replace(/^\/|\/$/g, ""));
			}
		});

	await rewriter.transform(new Response(Bun.file(join(build, "index.html")))).text();
	if (!siteUrl) throw new Error("Missing og:url in built index.html");
	for (const group of groups) group.title = clean(decode(group.title));

	return { siteUrl, origin: new URL(siteUrl).origin, basePath, groups };
}

async function readPage(
	slug: string,
	siteUrl: string,
	origin: string,
	basePath: string
): Promise<Page> {
	const file = join(build, slug, "index.html");
	const resolveHref = (href: string) => {
		const url = new URL(decode(href), siteUrl);
		if (url.origin !== origin || !url.pathname.startsWith(basePath)) return url.href;
		const rel = url.pathname.slice(basePath.length).replace(/^\/|\/$/g, "");
		return `${siteUrl}${rel || "index"}.md`;
	};

	let description = "";
	let title = "";
	const sections: { heading: string; blocks: string[] }[] = [];
	const section = () => sections[sections.length - 1];

	const stack: string[] = [];
	let raw = "";
	const flush = () => {
		if (!raw) return;
		if (stack.length > 0) stack[stack.length - 1] += decode(raw);
		raw = "";
	};
	const push = () => {
		flush();
		stack.push("");
	};
	const pop = () => {
		flush();
		return stack.pop() ?? "";
	};
	const write = (text: string) => {
		flush();
		if (stack.length > 0) stack[stack.length - 1] += text;
	};

	let skip = 0;
	let inPre = false;
	let inDd = false;
	let listKind: "ul" | "ol" = "ul";
	let listItems: string[] = [];
	let tableRows: string[] = [];
	let rowHead = "";
	let rowBody = "";
	let dlSetting = false;
	let dlItems: [string, string][] = [];
	let dlTerm = "";
	let kbdCount = 0;

	const skipElement = (el: HTMLRewriterTypes.Element) => {
		if (!el.canHaveContent) return;
		skip++;
		el.onEndTag(() => {
			skip--;
		});
	};

	const rewriter = new HTMLRewriter()
		.on('meta[name="description"]', {
			element(el) {
				description = el.getAttribute("content") ?? "";
			}
		})
		.on("article h1", {
			text({ text }) {
				title += text;
			}
		})
		.on("article section", {
			element() {
				sections.push({ heading: "", blocks: [] });
			}
		})
		.on("article section h2", {
			element: skipElement,
			text({ text }) {
				section().heading += text;
			}
		})
		.on(
			'article nav, article section button, article section fieldset, article section label, article section svg, article section [role="group"], article section .sr-only',
			{ element: skipElement }
		)
		.on("article section p", {
			element(el) {
				if (skip > 0) return;
				push();
				el.onEndTag(() => {
					const text = clean(pop());
					if (text) section().blocks.push(text);
				});
			}
		})
		.on("article section aside", {
			element(el) {
				if (skip > 0) return;
				push();
				el.onEndTag(() => {
					const text = clean(pop());
					if (text) section().blocks.push(`> ${text}`);
				});
			}
		})
		.on("article section pre", {
			element(el) {
				if (skip > 0) return;
				const lang = el.getAttribute("data-lang") ?? "";
				inPre = true;
				push();
				el.onEndTag(() => {
					inPre = false;
					const body = pop().trimEnd();
					const fence = body.includes("```") ? "````" : "```";
					section().blocks.push(`${fence}${lang}\n${body}\n${fence}`);
				});
			}
		})
		.on("article section ul, article section ol", {
			element(el) {
				if (skip > 0) return;
				listKind = el.tagName === "ol" ? "ol" : "ul";
				listItems = [];
				el.onEndTag(() => {
					if (listItems.length > 0) section().blocks.push(listItems.join("\n"));
				});
			}
		})
		.on("article section li", {
			element(el) {
				if (skip > 0) return;
				push();
				el.onEndTag(() => {
					const text = clean(pop());
					if (!text) return;
					const prefix = listKind === "ol" ? `${listItems.length + 1}.` : "-";
					listItems.push(`${prefix} ${text}`);
				});
			}
		})
		.on("article section table", {
			element(el) {
				if (skip > 0) return;
				tableRows = [];
				el.onEndTag(() => {
					if (tableRows.length > 0) section().blocks.push(tableRows.join("\n"));
				});
			}
		})
		.on("article section tr", {
			element(el) {
				if (skip > 0) return;
				rowHead = "";
				rowBody = "";
				el.onEndTag(() => {
					if (rowHead || rowBody) tableRows.push(`- **${rowHead}**: ${rowBody}`);
				});
			}
		})
		.on("article section th", {
			element(el) {
				if (skip > 0) return;
				push();
				el.onEndTag(() => {
					rowHead = clean(pop());
				});
			}
		})
		.on("article section td", {
			element(el) {
				if (skip > 0) return;
				push();
				el.onEndTag(() => {
					rowBody = clean(pop());
				});
			}
		})
		.on("article section dl", {
			element(el) {
				if (skip > 0) return;
				dlSetting = el.hasAttribute("data-setting");
				dlItems = [];
				dlTerm = "";
				el.onEndTag(() => {
					if (dlItems.length === 0) return;
					if (dlSetting && dlItems.length >= 3) {
						const [[, name], [, range], [, initial]] = dlItems;
						section().blocks.push(
							`**${codeSpan(name)}** (default: ${codeSpan(initial)}, range: ${codeSpan(range)})`
						);
						return;
					}
					section().blocks.push(dlItems.map(([term, def]) => `- ${term}: ${def}`).join("\n"));
				});
			}
		})
		.on("article section dt", {
			element(el) {
				if (skip > 0) return;
				push();
				el.onEndTag(() => {
					dlTerm = clean(pop());
				});
			}
		})
		.on("article section dd", {
			element(el) {
				if (skip > 0) return;
				inDd = true;
				kbdCount = 0;
				push();
				el.onEndTag(() => {
					inDd = false;
					dlItems.push([dlTerm, clean(pop())]);
					dlTerm = "";
				});
			}
		})
		.on("article section code", {
			element(el) {
				if (skip > 0 || inPre) return;
				push();
				el.onEndTag(() => {
					const text = clean(pop());
					if (text) write(codeSpan(text));
				});
			}
		})
		.on("article section kbd", {
			element(el) {
				if (skip > 0) return;
				if (inDd && kbdCount > 0) write(", ");
				if (inDd) kbdCount++;
				push();
				el.onEndTag(() => {
					const text = clean(pop());
					if (text) write(codeSpan(text));
				});
			}
		})
		.on("article section a", {
			element(el) {
				if (skip > 0) return;
				const href = el.getAttribute("href");
				const standalone = stack.length === 0;
				push();
				el.onEndTag(() => {
					const label = clean(pop());
					if (!label) return;
					const md = href ? `[${label}](${resolveHref(href)})` : label;
					if (standalone) section().blocks.push(md);
					else write(md);
				});
			}
		})
		.on("article section *", {
			text({ text }) {
				if (skip > 0 || stack.length === 0) return;
				raw += text;
			}
		});

	await rewriter.transform(new Response(Bun.file(file))).text();

	const cleanTitle = clean(decode(title));
	const cleanDescription = clean(decode(description));
	const pageUrl = slug ? `${siteUrl}${slug}/` : siteUrl;
	const lines = [`# ${cleanTitle}`];
	if (cleanDescription) lines.push("", `> ${cleanDescription}`);
	lines.push("", `Source: ${pageUrl}`);

	for (const { heading, blocks } of sections) {
		const cleanHeading = clean(decode(heading));
		if (!cleanHeading && blocks.length === 0) continue;
		lines.push("", `## ${cleanHeading}`);
		for (const block of blocks) lines.push("", block);
	}

	return {
		slug,
		title: cleanTitle,
		description: cleanDescription,
		markdown: `${lines.join("\n")}\n`
	};
}

const { siteUrl, origin, basePath, groups } = await readMenu();
const slugs = groups.flatMap((group) => group.slugs);
const pages = new Map<string, Page>();
for (const page of await Promise.all(
	slugs.map((slug) => readPage(slug, siteUrl, origin, basePath))
)) {
	pages.set(page.slug, page);
}

const home = pages.get("");
if (!home) throw new Error("Missing home page in menu");

const indexLines = [
	`# ${home.title}`,
	"",
	`> ${home.description}`,
	"",
	`Full documentation in one file: [llms-full.txt](${siteUrl}llms-full.txt)`
];

for (const group of groups) {
	indexLines.push("", `## ${group.title}`, "");
	for (const slug of group.slugs) {
		const page = pages.get(slug);
		if (!page) continue;
		const url = `${siteUrl}${slug || "index"}.md`;
		indexLines.push(`- [${page.title}](${url}): ${page.description}`);
	}
}

const llmsTxt = `${indexLines.join("\n")}\n`;
const llmsFullTxt = `${slugs.map((slug) => pages.get(slug)?.markdown.trimEnd()).join("\n\n---\n\n")}\n`;

for (const output of outputs) {
	await Bun.write(join(output, "llms.txt"), llmsTxt);
	await Bun.write(join(output, "llms-full.txt"), llmsFullTxt);
	for (const page of pages.values()) {
		await Bun.write(join(output, `${page.slug || "index"}.md`), page.markdown);
	}
}

console.log(`llms: ${pages.size} pages`);
