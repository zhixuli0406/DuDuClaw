import { type IntlFormatters, type Formatters, type CustomFormats, type OnErrorFn } from "./types.js";
export declare function formatRelativeTime(config: {
	locale: string;
	formats: CustomFormats;
	onError: OnErrorFn;
}, getRelativeTimeFormat: Formatters["getRelativeTimeFormat"], value: Parameters<IntlFormatters["formatRelativeTime"]>[0], unit?: Parameters<IntlFormatters["formatRelativeTime"]>[1], options?: Parameters<IntlFormatters["formatRelativeTime"]>[2]): string;
