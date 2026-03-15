export declare function GetStringOrBooleanOption<
	T extends object,
	K extends keyof T
>(opts: T, prop: K, values: T[K][] | undefined, trueValue: T[K] | boolean, falsyValue: T[K] | boolean, fallback: T[K] | boolean): T[K] | boolean;
