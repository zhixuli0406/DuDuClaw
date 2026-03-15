import { type CustomFormats, type Formatters, type IntlFormatters, type OnErrorFn } from "./types.js";
export declare function getFormatter({ locale, formats, onError, timeZone }: {
	locale: string;
	timeZone?: string;
	formats: CustomFormats;
	onError: OnErrorFn;
}, type: "date" | "time" | "dateTimeRange", getDateTimeFormat: Formatters["getDateTimeFormat"], options?: Parameters<IntlFormatters["formatDate"]>[1]): Intl.DateTimeFormat;
export declare function formatDate(config: {
	locale: string;
	timeZone?: string;
	formats: CustomFormats;
	onError: OnErrorFn;
}, getDateTimeFormat: Formatters["getDateTimeFormat"], value: Parameters<IntlFormatters["formatDate"]>[0], options?: Parameters<IntlFormatters["formatDate"]>[1]): string;
export declare function formatTime(config: {
	locale: string;
	timeZone?: string;
	formats: CustomFormats;
	onError: OnErrorFn;
}, getDateTimeFormat: Formatters["getDateTimeFormat"], value: Parameters<IntlFormatters["formatTime"]>[0], options?: Parameters<IntlFormatters["formatTime"]>[1]): string;
export declare function formatDateTimeRange(config: {
	locale: string;
	timeZone?: string;
	formats: CustomFormats;
	onError: OnErrorFn;
}, getDateTimeFormat: Formatters["getDateTimeFormat"], from: Parameters<IntlFormatters["formatDateTimeRange"]>[0], to: Parameters<IntlFormatters["formatDateTimeRange"]>[1], options?: Parameters<IntlFormatters["formatDateTimeRange"]>[2]): string;
export declare function formatDateToParts(config: {
	locale: string;
	timeZone?: string;
	formats: CustomFormats;
	onError: OnErrorFn;
}, getDateTimeFormat: Formatters["getDateTimeFormat"], value: Parameters<IntlFormatters["formatDate"]>[0], options?: Parameters<IntlFormatters["formatDate"]>[1]): Intl.DateTimeFormatPart[];
export declare function formatTimeToParts(config: {
	locale: string;
	timeZone?: string;
	formats: CustomFormats;
	onError: OnErrorFn;
}, getDateTimeFormat: Formatters["getDateTimeFormat"], value: Parameters<IntlFormatters["formatTimeToParts"]>[0], options?: Parameters<IntlFormatters["formatTimeToParts"]>[1]): Intl.DateTimeFormatPart[];
