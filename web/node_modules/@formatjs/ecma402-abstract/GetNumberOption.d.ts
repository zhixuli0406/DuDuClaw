export declare function GetNumberOption<
	T extends object,
	K extends keyof T,
	F extends number | undefined
>(options: T, property: K, minimum: number, maximum: number, fallback: F): F extends number ? number : number | undefined;
