import "@formatjs/intl";
import * as React from "react";
import useIntl from "./useIntl.js";
import { jsx as _jsx } from "react/jsx-runtime";
const FormattedDateTimeRange = (props) => {
	const intl = useIntl();
	const { from, to, children, ...formatProps } = props;
	const formattedValue = intl.formatDateTimeRange(from, to, formatProps);
	if (typeof children === "function") {
		return children(formattedValue);
	}
	const Text = intl.textComponent || React.Fragment;
	return /* @__PURE__ */ _jsx(Text, { children: formattedValue });
};
FormattedDateTimeRange.displayName = "FormattedDateTimeRange";
export default FormattedDateTimeRange;
