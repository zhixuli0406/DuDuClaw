import "intl-messageformat";
import * as React from "react";
import "./types.js";
import { DEFAULT_INTL_CONFIG as CORE_DEFAULT_INTL_CONFIG } from "@formatjs/intl";
import { jsx as _jsx } from "react/jsx-runtime";
export function invariant(condition, message, Err = Error) {
	if (!condition) {
		throw new Err(message);
	}
}
export function invariantIntlContext(intl) {
	invariant(intl, "[React Intl] Could not find required `intl` object. " + "<IntlProvider> needs to exist in the component ancestry.");
}
export const DEFAULT_INTL_CONFIG = {
	...CORE_DEFAULT_INTL_CONFIG,
	textComponent: React.Fragment
};
/**
* Builds an array of {@link React.ReactNode}s with index-based keys, similar to
* {@link React.Children.toArray}. However, this function tells React that it
* was intentional, so they won't produce a bunch of warnings about it.
*
* React doesn't recommend doing this because it makes reordering inefficient,
* but we mostly need this for message chunks, which don't tend to reorder to
* begin with.
*
*/
export const toKeyedReactNodeArray = (children) => {
	const childrenArray = React.Children.toArray(children);
	return childrenArray.map((child, index) => {
		// For React elements, wrap in a keyed Fragment
		// This creates a new element with a key rather than trying to add one after creation
		if (React.isValidElement(child)) {
			return /* @__PURE__ */ _jsx(React.Fragment, { children: child }, index);
		}
		return child;
	});
};
/**
* Takes a `formatXMLElementFn`, and composes it in function, which passes
* argument `parts` through, assigning unique key to each part, to prevent
* "Each child in a list should have a unique "key"" React error.
* @param formatXMLElementFn
*/
export function assignUniqueKeysToParts(formatXMLElementFn) {
	return function(parts) {
		// eslint-disable-next-line prefer-rest-params
		return formatXMLElementFn(toKeyedReactNodeArray(parts));
	};
}
export function shallowEqual(objA, objB) {
	if (objA === objB) {
		return true;
	}
	if (!objA || !objB) {
		return false;
	}
	var aKeys = Object.keys(objA);
	var bKeys = Object.keys(objB);
	var len = aKeys.length;
	if (bKeys.length !== len) {
		return false;
	}
	for (var i = 0; i < len; i++) {
		var key = aKeys[i];
		if (objA[key] !== objB[key] || !Object.prototype.hasOwnProperty.call(objB, key)) {
			return false;
		}
	}
	return true;
}
