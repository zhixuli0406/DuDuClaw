import "@formatjs/intl";
import * as React from "react";
import "../types.js";
import useIntl from "./useIntl.js";
import { jsx as _jsx } from "react/jsx-runtime";
var DisplayName = /* @__PURE__ */ function(DisplayName) {
	DisplayName["formatDate"] = "FormattedDate";
	DisplayName["formatTime"] = "FormattedTime";
	DisplayName["formatNumber"] = "FormattedNumber";
	DisplayName["formatList"] = "FormattedList";
	// Note that this DisplayName is the locale display name, not to be confused with
	// the name of the enum, which is for React component display name in dev tools.
	DisplayName["formatDisplayName"] = "FormattedDisplayName";
	return DisplayName;
}(DisplayName || {});
var DisplayNameParts = /* @__PURE__ */ function(DisplayNameParts) {
	DisplayNameParts["formatDate"] = "FormattedDateParts";
	DisplayNameParts["formatTime"] = "FormattedTimeParts";
	DisplayNameParts["formatNumber"] = "FormattedNumberParts";
	DisplayNameParts["formatList"] = "FormattedListParts";
	return DisplayNameParts;
}(DisplayNameParts || {});
export const FormattedNumberParts = (props) => {
	const intl = useIntl();
	const { value, children, ...formatProps } = props;
	return children(intl.formatNumberToParts(value, formatProps));
};
FormattedNumberParts.displayName = "FormattedNumberParts";
export const FormattedListParts = (props) => {
	const intl = useIntl();
	const { value, children, ...formatProps } = props;
	return children(intl.formatListToParts(value, formatProps));
};
FormattedNumberParts.displayName = "FormattedNumberParts";
export function createFormattedDateTimePartsComponent(name) {
	const ComponentParts = (props) => {
		const intl = useIntl();
		const { value, children, ...formatProps } = props;
		const date = typeof value === "string" ? new Date(value || 0) : value;
		const formattedParts = name === "formatDate" ? intl.formatDateToParts(date, formatProps) : intl.formatTimeToParts(date, formatProps);
		return children(formattedParts);
	};
	ComponentParts.displayName = DisplayNameParts[name];
	return ComponentParts;
}
export function createFormattedComponent(name) {
	const Component = (props) => {
		const intl = useIntl();
		const { value, children, ...formatProps } = props;
		// TODO: fix TS type definition for localeMatcher upstream
		const formattedValue = intl[name](value, formatProps);
		if (typeof children === "function") {
			return children(formattedValue);
		}
		const Text = intl.textComponent || React.Fragment;
		return /* @__PURE__ */ _jsx(Text, { children: formattedValue });
	};
	Component.displayName = DisplayName[name];
	return Component;
}
