import { ErrorCode, FormatError } from "intl-messageformat";
import { IntlFormatError } from "./error.js";
import "./types.js";
import { filterProps } from "./utils.js";
const LIST_FORMAT_OPTIONS = ["type", "style"];
const now = Date.now();
function generateToken(i) {
	return `${now}_${i}_${now}`;
}
export function formatList(opts, getListFormat, values, options = {}) {
	const results = formatListToParts(opts, getListFormat, values, options).reduce((all, el) => {
		const val = el.value;
		if (typeof val !== "string") {
			all.push(val);
		} else if (typeof all[all.length - 1] === "string") {
			all[all.length - 1] += val;
		} else {
			all.push(val);
		}
		return all;
	}, []);
	return results.length === 1 ? results[0] : results.length === 0 ? "" : results;
}
export function formatListToParts({ locale, onError }, getListFormat, values, options = {}) {
	const ListFormat = Intl.ListFormat;
	if (!ListFormat) {
		onError(new FormatError(`Intl.ListFormat is not available in this environment.
Try polyfilling it using "@formatjs/intl-listformat"
`, ErrorCode.MISSING_INTL_API));
	}
	const filteredOptions = filterProps(options, LIST_FORMAT_OPTIONS);
	try {
		const richValues = {};
		const serializedValues = Array.from(values).map((v, i) => {
			if (typeof v === "object" && v !== null) {
				const id = generateToken(i);
				richValues[id] = v;
				return id;
			}
			return String(v);
		});
		return getListFormat(locale, filteredOptions).formatToParts(serializedValues).map((part) => part.type === "literal" ? part : {
			...part,
			value: richValues[part.value] || part.value
		});
	} catch (e) {
		onError(new IntlFormatError("Error formatting list.", locale, e));
	}
	// @ts-ignore
	return values;
}
