import * as React from "react";
import "@formatjs/intl";
import { shallowEqual } from "../utils.js";
import useIntl from "./useIntl.js";
import { jsx as _jsx, Fragment as _Fragment } from "react/jsx-runtime";
function areEqual(prevProps, nextProps) {
	const { values, ...otherProps } = prevProps;
	const { values: nextValues, ...nextOtherProps } = nextProps;
	return shallowEqual(nextValues, values) && shallowEqual(otherProps, nextOtherProps);
}
function FormattedMessage(props) {
	const intl = useIntl();
	const { formatMessage, textComponent: Text = React.Fragment } = intl;
	const { id, description, defaultMessage, values, children, tagName: Component = Text, ignoreTag } = props;
	const descriptor = {
		id,
		description,
		defaultMessage
	};
	const nodes = formatMessage(descriptor, values, { ignoreTag });
	if (typeof children === "function") {
		return children(Array.isArray(nodes) ? nodes : [nodes]);
	}
	if (Component) {
		return /* @__PURE__ */ _jsx(Component, { children: nodes });
	}
	return /* @__PURE__ */ _jsx(_Fragment, { children: nodes });
}
FormattedMessage.displayName = "FormattedMessage";
const MemoizedFormattedMessage = React.memo(FormattedMessage, areEqual);
MemoizedFormattedMessage.displayName = "MemoizedFormattedMessage";
export default MemoizedFormattedMessage;
