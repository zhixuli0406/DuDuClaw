import { type Formatters, type IntlFormatters, type OnErrorFn } from "./types.js";
export declare function formatPlural({ locale, onError }: {
	locale: string;
	onError: OnErrorFn;
}, getPluralRules: Formatters["getPluralRules"], value: Parameters<IntlFormatters["formatPlural"]>[0], options?: Parameters<IntlFormatters["formatPlural"]>[1]): Intl.LDMLPluralRule;
