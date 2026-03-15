import { type Formatters, type IntlFormatters, type OnErrorFn, type Part } from "./types.js";
export declare function formatList(opts: {
	locale: string;
	onError: OnErrorFn;
}, getListFormat: Formatters["getListFormat"], values: Iterable<string>, options: Parameters<IntlFormatters["formatList"]>[1]): string;
export declare function formatListToParts<T>(opts: {
	locale: string;
	onError: OnErrorFn;
}, getListFormat: Formatters["getListFormat"], values: Iterable<string | T>, options: Parameters<IntlFormatters["formatList"]>[1]): Part[];
