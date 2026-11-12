const query = "(prefers-color-scheme: dark)";

export const isDark = () => {
	const theme = document.documentElement.dataset.theme;
	return theme ? theme === "dark" : matchMedia(query).matches;
};

export function watchTheme(callback: (dark: boolean) => void) {
	const notify = () => callback(isDark());
	const observer = new MutationObserver(notify);
	observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
	const media = matchMedia(query);
	media.addEventListener("change", notify);
	return () => {
		observer.disconnect();
		media.removeEventListener("change", notify);
	};
}
