import "@formatjs/ecma402-abstract";
import { isArgumentElement, isDateElement, isDateTimeSkeleton, isLiteralElement, isNumberElement, isNumberSkeleton, isPluralElement, isPoundElement, isSelectElement, isTagElement, isTimeElement } from "@formatjs/icu-messageformat-parser";
import { ErrorCode, FormatError, InvalidValueError, InvalidValueTypeError, MissingValueError } from "./error.js";
export let PART_TYPE = /* @__PURE__ */ function(PART_TYPE) {
	PART_TYPE[PART_TYPE["literal"] = 0] = "literal";
	PART_TYPE[PART_TYPE["object"] = 1] = "object";
	return PART_TYPE;
}({});
function mergeLiteral(parts) {
	if (parts.length < 2) {
		return parts;
	}
	return parts.reduce((all, part) => {
		const lastPart = all[all.length - 1];
		if (!lastPart || lastPart.type !== PART_TYPE.literal || part.type !== PART_TYPE.literal) {
			all.push(part);
		} else {
			lastPart.value += part.value;
		}
		return all;
	}, []);
}
export function isFormatXMLElementFn(el) {
	return typeof el === "function";
}
// TODO(skeleton): add skeleton support
export function formatToParts(els, locales, formatters, formats, values, currentPluralValue, originalMessage) {
	// Hot path for straight simple msg translations
	if (els.length === 1 && isLiteralElement(els[0])) {
		return [{
			type: PART_TYPE.literal,
			value: els[0].value
		}];
	}
	const result = [];
	for (const el of els) {
		// Exit early for string parts.
		if (isLiteralElement(el)) {
			result.push({
				type: PART_TYPE.literal,
				value: el.value
			});
			continue;
		}
		// TODO: should this part be literal type?
		// Replace `#` in plural rules with the actual numeric value.
		if (isPoundElement(el)) {
			if (typeof currentPluralValue === "number") {
				result.push({
					type: PART_TYPE.literal,
					value: formatters.getNumberFormat(locales).format(currentPluralValue)
				});
			}
			continue;
		}
		const { value: varName } = el;
		// Enforce that all required values are provided by the caller.
		if (!(values && varName in values)) {
			throw new MissingValueError(varName, originalMessage);
		}
		let value = values[varName];
		if (isArgumentElement(el)) {
			if (!value || typeof value === "string" || typeof value === "number" || typeof value === "bigint") {
				value = typeof value === "string" || typeof value === "number" || typeof value === "bigint" ? String(value) : "";
			}
			result.push({
				type: typeof value === "string" ? PART_TYPE.literal : PART_TYPE.object,
				value
			});
			continue;
		}
		// Recursively format plural and select parts' option — which can be a
		// nested pattern structure. The choosing of the option to use is
		// abstracted-by and delegated-to the part helper object.
		if (isDateElement(el)) {
			const style = typeof el.style === "string" ? formats.date[el.style] : isDateTimeSkeleton(el.style) ? el.style.parsedOptions : undefined;
			result.push({
				type: PART_TYPE.literal,
				value: formatters.getDateTimeFormat(locales, style).format(value)
			});
			continue;
		}
		if (isTimeElement(el)) {
			const style = typeof el.style === "string" ? formats.time[el.style] : isDateTimeSkeleton(el.style) ? el.style.parsedOptions : formats.time.medium;
			result.push({
				type: PART_TYPE.literal,
				value: formatters.getDateTimeFormat(locales, style).format(value)
			});
			continue;
		}
		if (isNumberElement(el)) {
			const style = typeof el.style === "string" ? formats.number[el.style] : isNumberSkeleton(el.style) ? el.style.parsedOptions : undefined;
			if (style && style.scale) {
				const scale = style.scale || 1;
				// Handle bigint scale multiplication
				// BigInt can only be multiplied by BigInt
				if (typeof value === "bigint") {
					// Check if scale is a safe integer that can be converted to BigInt
					if (!Number.isInteger(scale)) {
						throw new TypeError(`Cannot apply fractional scale ${scale} to bigint value. Scale must be an integer when formatting bigint.`);
					}
					value = value * BigInt(scale);
				} else {
					value = value * scale;
				}
			}
			result.push({
				type: PART_TYPE.literal,
				value: formatters.getNumberFormat(locales, style).format(value)
			});
			continue;
		}
		if (isTagElement(el)) {
			const { children, value } = el;
			const formatFn = values[value];
			if (!isFormatXMLElementFn(formatFn)) {
				throw new InvalidValueTypeError(value, "function", originalMessage);
			}
			const parts = formatToParts(children, locales, formatters, formats, values, currentPluralValue);
			let chunks = formatFn(parts.map((p) => p.value));
			if (!Array.isArray(chunks)) {
				chunks = [chunks];
			}
			result.push(...chunks.map((c) => {
				return {
					type: typeof c === "string" ? PART_TYPE.literal : PART_TYPE.object,
					value: c
				};
			}));
		}
		if (isSelectElement(el)) {
			// GH #4490: Use hasOwnProperty to avoid prototype chain issues with keys like "constructor"
			const key = value;
			const opt = (Object.prototype.hasOwnProperty.call(el.options, key) ? el.options[key] : undefined) || el.options.other;
			if (!opt) {
				throw new InvalidValueError(el.value, value, Object.keys(el.options), originalMessage);
			}
			result.push(...formatToParts(opt.value, locales, formatters, formats, values));
			continue;
		}
		if (isPluralElement(el)) {
			// GH #4490: Use hasOwnProperty to avoid prototype chain issues
			const exactKey = `=${value}`;
			let opt = Object.prototype.hasOwnProperty.call(el.options, exactKey) ? el.options[exactKey] : undefined;
			if (!opt) {
				if (!Intl.PluralRules) {
					throw new FormatError(`Intl.PluralRules is not available in this environment.
Try polyfilling it using "@formatjs/intl-pluralrules"
`, ErrorCode.MISSING_INTL_API, originalMessage);
				}
				// Convert bigint to number for PluralRules (which only accepts number)
				const numericValue = typeof value === "bigint" ? Number(value) : value;
				const rule = formatters.getPluralRules(locales, { type: el.pluralType }).select(numericValue - (el.offset || 0));
				opt = (Object.prototype.hasOwnProperty.call(el.options, rule) ? el.options[rule] : undefined) || el.options.other;
			}
			if (!opt) {
				throw new InvalidValueError(el.value, value, Object.keys(el.options), originalMessage);
			}
			// Convert bigint to number for currentPluralValue
			const numericValue = typeof value === "bigint" ? Number(value) : value;
			result.push(...formatToParts(opt.value, locales, formatters, formats, values, numericValue - (el.offset || 0)));
			continue;
		}
	}
	return mergeLiteral(result);
}
