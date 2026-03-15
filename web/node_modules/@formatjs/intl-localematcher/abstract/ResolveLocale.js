import { BestFitMatcher } from "./BestFitMatcher.js";
import { CanonicalizeUValue } from "./CanonicalizeUValue.js";
import { InsertUnicodeExtensionAndCanonicalize } from "./InsertUnicodeExtensionAndCanonicalize.js";
import { LookupMatcher } from "./LookupMatcher.js";
import { UnicodeExtensionComponents } from "./UnicodeExtensionComponents.js";
import { invariant } from "./utils.js";
/**
* https://tc39.es/ecma402/#sec-resolvelocale
*/
export function ResolveLocale(availableLocales, requestedLocales, options, relevantExtensionKeys, localeData, getDefaultLocale) {
	const matcher = options.localeMatcher;
	let r;
	if (matcher === "lookup") {
		r = LookupMatcher(Array.from(availableLocales), requestedLocales, getDefaultLocale);
	} else {
		r = BestFitMatcher(Array.from(availableLocales), requestedLocales, getDefaultLocale);
	}
	if (r == null) {
		r = {
			locale: getDefaultLocale(),
			extension: ""
		};
	}
	let foundLocale = r.locale;
	let foundLocaleData = localeData[foundLocale];
	// TODO: We can't really guarantee that the locale data is available
	// invariant(
	//   foundLocaleData !== undefined,
	//   `Missing locale data for ${foundLocale}`
	// )
	const result = {
		locale: "en",
		dataLocale: foundLocale
	};
	let components;
	let keywords;
	if (r.extension) {
		components = UnicodeExtensionComponents(r.extension);
		keywords = components.keywords;
	} else {
		keywords = [];
	}
	let supportedKeywords = [];
	for (const key of relevantExtensionKeys) {
		// TODO: Shouldn't default to empty array, see TODO above
		let keyLocaleData = foundLocaleData?.[key] ?? [];
		invariant(Array.isArray(keyLocaleData), `keyLocaleData for ${key} must be an array`);
		let value = keyLocaleData[0];
		invariant(value === undefined || typeof value === "string", `value must be a string or undefined`);
		let supportedKeyword;
		let entry = keywords.find((k) => k.key === key);
		if (entry) {
			let requestedValue = entry.value;
			if (requestedValue !== "") {
				if (keyLocaleData.indexOf(requestedValue) > -1) {
					value = requestedValue;
					supportedKeyword = {
						key,
						value
					};
				}
			} else if (keyLocaleData.indexOf("true") > -1) {
				value = "true";
				supportedKeyword = {
					key,
					value
				};
			}
		}
		let optionsValue = options[key];
		invariant(optionsValue == null || typeof optionsValue === "string", `optionsValue must be a string or undefined`);
		if (typeof optionsValue === "string") {
			let ukey = key.toLowerCase();
			optionsValue = CanonicalizeUValue(ukey, optionsValue);
			if (optionsValue === "") {
				optionsValue = "true";
			}
		}
		if (optionsValue !== value && keyLocaleData.indexOf(optionsValue) > -1) {
			value = optionsValue;
			supportedKeyword = undefined;
		}
		if (supportedKeyword) {
			supportedKeywords.push(supportedKeyword);
		}
		result[key] = value;
	}
	let supportedAttributes = [];
	if (supportedKeywords.length > 0) {
		supportedAttributes = [];
		foundLocale = InsertUnicodeExtensionAndCanonicalize(foundLocale, supportedAttributes, supportedKeywords);
	}
	result.locale = foundLocale;
	return result;
}
