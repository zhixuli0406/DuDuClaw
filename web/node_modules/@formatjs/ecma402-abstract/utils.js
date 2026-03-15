import { memoize, strategies } from "@formatjs/fast-memoize";
export function repeat(s, times) {
	if (typeof s.repeat === "function") {
		return s.repeat(times);
	}
	const arr = Array.from({ length: times });
	for (let i = 0; i < arr.length; i++) {
		arr[i] = s;
	}
	return arr.join("");
}
export function setInternalSlot(map, pl, field, value) {
	if (!map.get(pl)) {
		map.set(pl, Object.create(null));
	}
	const slots = map.get(pl);
	slots[field] = value;
}
export function setMultiInternalSlots(map, pl, props) {
	for (const k of Object.keys(props)) {
		setInternalSlot(map, pl, k, props[k]);
	}
}
export function getInternalSlot(map, pl, field) {
	return getMultiInternalSlots(map, pl, field)[field];
}
export function getMultiInternalSlots(map, pl, ...fields) {
	const slots = map.get(pl);
	if (!slots) {
		throw new TypeError(`${pl} InternalSlot has not been initialized`);
	}
	return fields.reduce((all, f) => {
		all[f] = slots[f];
		return all;
	}, Object.create(null));
}
export function isLiteralPart(patternPart) {
	return patternPart.type === "literal";
}
/*
17 ECMAScript Standard Built-in Objects:
Every built-in Function object, including constructors, that is not
identified as an anonymous function has a name property whose value
is a String.

Unless otherwise specified, the name property of a built-in Function
object, if it exists, has the attributes { [[Writable]]: false,
[[Enumerable]]: false, [[Configurable]]: true }.
*/
export function defineProperty(target, name, { value }) {
	Object.defineProperty(target, name, {
		configurable: true,
		enumerable: false,
		writable: true,
		value
	});
}
/**
* 7.3.5 CreateDataProperty
* @param target
* @param name
* @param value
*/
export function createDataProperty(target, name, value) {
	Object.defineProperty(target, name, {
		configurable: true,
		enumerable: true,
		writable: true,
		value
	});
}
export const UNICODE_EXTENSION_SEQUENCE_REGEX = /-u(?:-[0-9a-z]{2,8})+/gi;
export function invariant(condition, message, Err = Error) {
	if (!condition) {
		throw new Err(message);
	}
}
export const createMemoizedNumberFormat = memoize((...args) => new Intl.NumberFormat(...args), { strategy: strategies.variadic });
export const createMemoizedPluralRules = memoize((...args) => new Intl.PluralRules(...args), { strategy: strategies.variadic });
export const createMemoizedLocale = memoize((...args) => new Intl.Locale(...args), { strategy: strategies.variadic });
export const createMemoizedListFormat = memoize((...args) => new Intl.ListFormat(...args), { strategy: strategies.variadic });
