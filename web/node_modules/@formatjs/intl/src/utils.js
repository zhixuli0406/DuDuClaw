import "@formatjs/ecma402-abstract";
import { memoize, strategies } from "@formatjs/fast-memoize";
import { IntlMessageFormat } from "intl-messageformat";
import { UnsupportedFormatterError } from "./error.js";
import "./types.js";
export function invariant(condition, message, Err = Error) {
	if (!condition) {
		throw new Err(message);
	}
}
export function filterProps(props, allowlist, defaults = {}) {
	return allowlist.reduce((filtered, name) => {
		if (name in props) {
			filtered[name] = props[name];
		} else if (name in defaults) {
			filtered[name] = defaults[name];
		}
		return filtered;
	}, {});
}
const defaultErrorHandler = (error) => {
	// @ts-ignore just so we don't need to declare dep on @types/node
	if (process.env.NODE_ENV !== "production") {
		console.error(error);
	}
};
const defaultWarnHandler = (warning) => {
	// @ts-ignore just so we don't need to declare dep on @types/node
	if (process.env.NODE_ENV !== "production") {
		console.warn(warning);
	}
};
export const DEFAULT_INTL_CONFIG = {
	formats: {},
	messages: {},
	timeZone: undefined,
	defaultLocale: "en",
	defaultFormats: {},
	fallbackOnEmptyString: true,
	onError: defaultErrorHandler,
	onWarn: defaultWarnHandler
};
export function createIntlCache() {
	return {
		dateTime: {},
		number: {},
		message: {},
		relativeTime: {},
		pluralRules: {},
		list: {},
		displayNames: {}
	};
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
/**
* Create intl formatters and populate cache
* @param cache explicit cache to prevent leaking memory
*/
export function createFormatters(cache = createIntlCache()) {
	const RelativeTimeFormat = Intl.RelativeTimeFormat;
	const ListFormat = Intl.ListFormat;
	const DisplayNames = Intl.DisplayNames;
	const getDateTimeFormat = memoize((...args) => new Intl.DateTimeFormat(...args), {
		cache: createFastMemoizeCache(cache.dateTime),
		strategy: strategies.variadic
	});
	const getNumberFormat = memoize((...args) => new Intl.NumberFormat(...args), {
		cache: createFastMemoizeCache(cache.number),
		strategy: strategies.variadic
	});
	const getPluralRules = memoize((...args) => new Intl.PluralRules(...args), {
		cache: createFastMemoizeCache(cache.pluralRules),
		strategy: strategies.variadic
	});
	return {
		getDateTimeFormat,
		getNumberFormat,
		getMessageFormat: memoize((message, locales, overrideFormats, opts) => new IntlMessageFormat(message, locales, overrideFormats, {
			formatters: {
				getNumberFormat,
				getDateTimeFormat,
				getPluralRules
			},
			...opts
		}), {
			cache: createFastMemoizeCache(cache.message),
			strategy: strategies.variadic
		}),
		getRelativeTimeFormat: memoize((...args) => new RelativeTimeFormat(...args), {
			cache: createFastMemoizeCache(cache.relativeTime),
			strategy: strategies.variadic
		}),
		getPluralRules,
		getListFormat: memoize((...args) => new ListFormat(...args), {
			cache: createFastMemoizeCache(cache.list),
			strategy: strategies.variadic
		}),
		getDisplayNames: memoize((...args) => new DisplayNames(...args), {
			cache: createFastMemoizeCache(cache.displayNames),
			strategy: strategies.variadic
		})
	};
}
export function getNamedFormat(formats, type, name, onError) {
	const formatType = formats && formats[type];
	let format;
	if (formatType) {
		format = formatType[name];
	}
	if (format) {
		return format;
	}
	onError(new UnsupportedFormatterError(`No ${type} format named: ${name}`));
}
