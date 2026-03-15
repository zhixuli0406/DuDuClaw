import { type Formatters, type IntlFormatters, type OnErrorFn } from "./types.js";
export declare function formatDisplayName({ locale, onError }: {
	locale: string;
	onError: OnErrorFn;
}, getDisplayNames: Formatters["getDisplayNames"], value: Parameters<IntlFormatters["formatDisplayName"]>[0], options: Parameters<IntlFormatters["formatDisplayName"]>[1]): string | undefined;
