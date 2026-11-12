<script lang="ts">
	import { afterNavigate } from "$app/navigation";
	import { resolve } from "$app/paths";
	import { page } from "$app/state";
	import { groups, route } from "#lib/pages.ts";

	let menu: HTMLElement;
	afterNavigate(() => menu.hidePopover());
</script>

<nav
	id="menu"
	popover
	bind:this={menu}
	aria-label="Pages"
	class="fixed start-0 top-(--header) m-0 h-[calc(100dvh-var(--header))] w-72 max-w-[85vw] overflow-y-auto border-0 border-e border-seam bg-void p-4 text-ink backdrop:bg-black/40 md:sticky md:block md:w-60 md:max-w-none md:shrink-0 md:self-start md:px-4 md:py-6"
>
	{#each groups as group (group.title)}
		<section class="mb-6">
			<h2 class="mb-1 px-2 text-xs font-medium tracking-wide text-faint uppercase">
				{group.title}
			</h2>
			<ul>
				{#each group.pages as entry (entry.slug)}
					<li>
						<a
							href={resolve(route(entry))}
							aria-current={page.route.id === route(entry) ? "page" : undefined}
							class="relative block cursor-pointer rounded-sm px-2 py-1 text-sm pointer-coarse:py-2.5 text-muted before:absolute before:inset-y-1 before:start-0 before:w-0.5 hover:text-ink aria-[current=page]:bg-raised aria-[current=page]:font-medium aria-[current=page]:text-ink aria-[current=page]:before:bg-accent"
						>
							{entry.title}
						</a>
					</li>
				{/each}
			</ul>
		</section>
	{/each}
</nav>
