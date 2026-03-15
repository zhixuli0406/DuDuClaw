/*
Copyright (c) 2014, Yahoo! Inc. All rights reserved.
Copyrights licensed under the New BSD License.
See the accompanying LICENSE file for terms.
*/
import { memoize, strategies } from "@formatjs/fast-memoize";
import { parse } from "@formatjs/icu-messageformat-parser";
import { formatToParts, PART_TYPE } from "./formatters.js";
// -- MessageFormat --------------------------------------------------------
function mergeConfig(c1, c2) {
	if (!c2) {
		return c1;
	}
	return {
		...c1,
		...c2,
		...Object.keys(c1).reduce((all, k) => {
			all[k] = {
				...c1[k],
				...c2[k]
			};
			return all;
		}, {})
	};
}
function mergeConfigs(defaultConfig, configs) {
	if (!configs) {
		return defaultConfig;
	}
	return Object.keys(defaultConfig).reduce((all, k) => {
		all[k] = mergeConfig(defaultConfig[k], configs[k]);
		return all;
	}, { ...defaultConfig });
}
function createFastMemoizeCache(store) {
	return { create() {
		return {
			get(key) {
				return store[key];
			},
			set(key, value) {
				store[key] = value;
			}
		};
	} };
}
function createDefaultFormatters(cache = {
	number: {},
	dateTime: {},
	pluralRules: {}
}) {
	return {
		getNumberFormat: memoize((...args) => new Intl.NumberFormat(...args), {
			cache: createFastMemoizeCache(cache.number),
			strategy: strategies.variadic
		}),
		getDateTimeFormat: memoize((...args) => new Intl.DateTimeFormat(...args), {
			cache: createFastMemoizeCache(cache.dateTime),
			strategy: strategies.variadic
		}),
		getPluralRules: memoize((...args) => new Intl.PluralRules(...args), {
			cache: createFastMemoizeCache(cache.pluralRules),
			strategy: strategies.variadic
		})
	};
}
export class IntlMessageFormat {
	ast;
	locales;
	resolvedLocale;
	formatters;
	formats;
	message;
	formatterCache = {
		number: {},
		dateTime: {},
		pluralRules: {}
	};
	constructor(message, locales = IntlMessageFormat.defaultLocale, overrideFormats, opts) {
		// Defined first because it's used to build the format pattern.
		this.locales = locales;
		this.resolvedLocale = IntlMessageFormat.resolveLocale(locales);
		if (typeof message === "string") {
			this.message = message;
			if (!IntlMessageFormat.__parse) {
				throw new TypeError("IntlMessageFormat.__parse must be set to process `message` of type `string`");
			}
			const { ...parseOpts } = opts || {};
			// Parse string messages into an AST.
			this.ast = IntlMessageFormat.__parse(message, {
				...parseOpts,
				locale: this.resolvedLocale
			});
		} else {
			this.ast = message;
		}
		if (!Array.isArray(this.ast)) {
			throw new TypeError("A message must be provided as a String or AST.");
		}
		// Creates a new object with the specified `formats` merged with the default
		// formats.
		this.formats = mergeConfigs(IntlMessageFormat.formats, overrideFormats);
		this.formatters = opts && opts.formatters || createDefaultFormatters(this.formatterCache);
	}
	format = (values) => {
		const parts = this.formatToParts(values);
		// Hot path for straight simple msg translations
		if (parts.length === 1) {
			return parts[0].value;
		}
		const result = parts.reduce((all, part) => {
			if (!all.length || part.type !== PART_TYPE.literal || typeof all[all.length - 1] !== "string") {
				all.push(part.value);
			} else {
				all[all.length - 1] += part.value;
			}
			return all;
		}, []);
		if (result.length <= 1) {
			return result[0] || "";
		}
		return result;
	};
	formatToParts = (values) => formatToParts(this.ast, this.locales, this.formatters, this.formats, values, undefined, this.message);
	resolvedOptions = () => ({ locale: this.resolvedLocale?.toString() || Intl.NumberFormat.supportedLocalesOf(this.locales)[0] });
	getAst = () => this.ast;
	static memoizedDefaultLocale = null;
	static get defaultLocale() {
		if (!IntlMessageFormat.memoizedDefaultLocale) {
			IntlMessageFormat.memoizedDefaultLocale = new Intl.NumberFormat().resolvedOptions().locale;
		}
		return IntlMessageFormat.memoizedDefaultLocale;
	}
	static resolveLocale = (locales) => {
		if (typeof Intl.Locale === "undefined") {
			return;
		}
		const supportedLocales = Intl.NumberFormat.supportedLocalesOf(locales);
		if (supportedLocales.length > 0) {
			return new Intl.Locale(supportedLocales[0]);
		}
		return new Intl.Locale(typeof locales === "string" ? locales : locales[0]);
	};
	static __parse = parse;
	// Default format options used as the prototype of the `formats` provided to the
	// constructor. These are used when constructing the internal Intl.NumberFormat
	// and Intl.DateTimeFormat instances.
	static formats = {
		number: {
			integer: { maximumFractionDigits: 0 },
			currency: { style: "currency" },
			percent: { style: "percent" }
		},
		date: {
			short: {
				month: "numeric",
				day: "numeric",
				year: "2-digit"
			},
			medium: {
				month: "short",
				day: "numeric",
				year: "numeric"
			},
			long: {
				month: "long",
				day: "numeric",
				year: "numeric"
			},
			full: {
				weekday: "long",
				month: "long",
				day: "numeric",
				year: "numeric"
			}
		},
		time: {
			short: {
				hour: "numeric",
				minute: "numeric"
			},
			medium: {
				hour: "numeric",
				minute: "numeric",
				second: "numeric"
			},
			long: {
				hour: "numeric",
				minute: "numeric",
				second: "numeric",
				timeZoneName: "short"
			},
			full: {
				hour: "numeric",
				minute: "numeric",
				second: "numeric",
				timeZoneName: "short"
			}
		}
	};
}
