import "@formatjs/icu-skeleton-parser";
import { isArgumentElement, isDateElement, isLiteralElement, isNumberElement, isPluralElement, isPoundElement, isSelectElement, isTagElement, isTimeElement, SKELETON_TYPE, TYPE } from "./types.js";
export function printAST(ast) {
	return doPrintAST(ast, false);
}
export function doPrintAST(ast, isInPlural) {
	const printedNodes = ast.map((el, i) => {
		if (isLiteralElement(el)) {
			return printLiteralElement(el, isInPlural, i === 0, i === ast.length - 1);
		}
		if (isArgumentElement(el)) {
			return printArgumentElement(el);
		}
		if (isDateElement(el) || isTimeElement(el) || isNumberElement(el)) {
			return printSimpleFormatElement(el);
		}
		if (isPluralElement(el)) {
			return printPluralElement(el);
		}
		if (isSelectElement(el)) {
			return printSelectElement(el);
		}
		if (isPoundElement(el)) {
			return "#";
		}
		if (isTagElement(el)) {
			return printTagElement(el);
		}
	});
	return printedNodes.join("");
}
function printTagElement(el) {
	return `<${el.value}>${printAST(el.children)}</${el.value}>`;
}
function printEscapedMessage(message) {
	return message.replace(/([{}](?:[\s\S]*[{}])?)/, `'$1'`);
}
function printLiteralElement({ value }, isInPlural, isFirstEl, isLastEl) {
	let escaped = value;
	// If this literal starts with a ' and its not the 1st node, this means the node before it is non-literal
	// and the `'` needs to be unescaped
	if (!isFirstEl && escaped[0] === `'`) {
		escaped = `''${escaped.slice(1)}`;
	}
	// Same logic but for last el
	if (!isLastEl && escaped[escaped.length - 1] === `'`) {
		escaped = `${escaped.slice(0, escaped.length - 1)}''`;
	}
	escaped = printEscapedMessage(escaped);
	return isInPlural ? escaped.replace("#", "'#'") : escaped;
}
function printArgumentElement({ value }) {
	return `{${value}}`;
}
function printSimpleFormatElement(el) {
	return `{${el.value}, ${TYPE[el.type]}${el.style ? `, ${printArgumentStyle(el.style)}` : ""}}`;
}
function printNumberSkeletonToken(token) {
	const { stem, options } = token;
	return options.length === 0 ? stem : `${stem}${options.map((o) => `/${o}`).join("")}`;
}
function printArgumentStyle(style) {
	if (typeof style === "string") {
		return printEscapedMessage(style);
	} else if (style.type === SKELETON_TYPE.dateTime) {
		return `::${printDateTimeSkeleton(style)}`;
	} else {
		return `::${style.tokens.map(printNumberSkeletonToken).join(" ")}`;
	}
}
export function printDateTimeSkeleton(style) {
	return style.pattern;
}
function printSelectElement(el) {
	const msg = [
		el.value,
		"select",
		Object.keys(el.options).map((id) => `${id}{${doPrintAST(el.options[id].value, false)}}`).join(" ")
	].join(",");
	return `{${msg}}`;
}
function printPluralElement(el) {
	const type = el.pluralType === "cardinal" ? "plural" : "selectordinal";
	const msg = [
		el.value,
		type,
		[el.offset ? `offset:${el.offset}` : "", ...Object.keys(el.options).map((id) => `${id}{${doPrintAST(el.options[id].value, true)}}`)].filter(Boolean).join(" ")
	].join(",");
	return `{${msg}}`;
}
