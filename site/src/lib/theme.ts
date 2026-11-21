const query = "(prefers-color-scheme: dark)";
const listeners = new Set<(dark: boolean) => void>();

export const isDark = () => {
	const theme = document.documentElement.dataset.theme;
	return theme ? theme === "dark" : matchMedia(query).matches;
};

export function applyTheme(theme: "light" | "dark") {
	document.documentElement.dataset.theme = theme;
	const dark = theme === "dark";
	for (const listener of listeners) listener(dark);
	try {
		localStorage.setItem("theme", theme);
	} catch (error) {
		console.error(error);
	}
}

export function watchTheme(callback: (dark: boolean) => void) {
	listeners.add(callback);
	const notify = () => callback(isDark());
	const media = matchMedia(query);
	media.addEventListener("change", notify);
	return () => {
		listeners.delete(callback);
		media.removeEventListener("change", notify);
	};
}
