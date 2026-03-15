import { invariant } from "./utils.js";
export function CanonicalizeUValue(ukey, uvalue) {
	// TODO: Implement algorithm for CanonicalizeUValue per https://tc39.es/ecma402/#sec-canonicalizeuvalue
	let lowerValue = uvalue.toLowerCase();
	invariant(ukey !== undefined, `ukey must be defined`);
	let canonicalized = lowerValue;
	return canonicalized;
}
