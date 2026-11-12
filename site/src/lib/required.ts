export const required = <T>(value: T | null): T => {
	if (value === null) throw new Error("A required value is missing");
	return value;
};
