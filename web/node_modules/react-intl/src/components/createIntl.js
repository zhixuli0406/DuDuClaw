/*
* Copyright 2015, Yahoo Inc.
* Copyrights licensed under the New BSD License.
* See the accompanying LICENSE file for terms.
*/
import { createIntl as coreCreateIntl, formatMessage as coreFormatMessage } from "@formatjs/intl";
import { isFormatXMLElementFn } from "intl-messageformat";
import * as React from "react";
import { DEFAULT_INTL_CONFIG, assignUniqueKeysToParts, toKeyedReactNodeArray } from "../utils.js";
function assignUniqueKeysToFormatXMLElementFnArgument(values) {
	if (!values) {
		return values;
	}
	return Object.keys(values).reduce((acc, k) => {
		const v = values[k];
		acc[k] = isFormatXMLElementFn(v) ? assignUniqueKeysToParts(v) : v;
		return acc;
	}, {});
}
const formatMessage = (config, formatters, descriptor, rawValues, ...rest) => {
	const values = assignUniqueKeysToFormatXMLElementFnArgument(rawValues);
	const chunks = coreFormatMessage(config, formatters, descriptor, values, ...rest);
	if (Array.isArray(chunks)) {
		return toKeyedReactNodeArray(chunks);
	}
	return chunks;
};
/**
* Create intl object
* @param config intl config
* @param cache cache for formatter instances to prevent memory leak
*/
export const createIntl = ({ defaultRichTextElements: rawDefaultRichTextElements, ...config }, cache) => {
	const defaultRichTextElements = assignUniqueKeysToFormatXMLElementFnArgument(rawDefaultRichTextElements);
	const coreIntl = coreCreateIntl({
		...DEFAULT_INTL_CONFIG,
		...config,
		defaultRichTextElements
	}, cache);
	const resolvedConfig = {
		locale: coreIntl.locale,
		timeZone: coreIntl.timeZone,
		fallbackOnEmptyString: coreIntl.fallbackOnEmptyString,
		formats: coreIntl.formats,
		defaultLocale: coreIntl.defaultLocale,
		defaultFormats: coreIntl.defaultFormats,
		messages: coreIntl.messages,
		onError: coreIntl.onError,
		defaultRichTextElements
	};
	return {
		...coreIntl,
		formatMessage: formatMessage.bind(null, resolvedConfig, coreIntl.formatters),
		$t: formatMessage.bind(null, resolvedConfig, coreIntl.formatters)
	};
};
