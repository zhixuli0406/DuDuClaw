/*
* Copyright 2015, Yahoo Inc.
* Copyrights licensed under the New BSD License.
* See the accompanying LICENSE file for terms.
*/
import * as React from "react";
import "@formatjs/intl";
import useIntl from "./useIntl.js";
import { jsx as _jsx } from "react/jsx-runtime";
const FormattedPlural = (props) => {
	const { formatPlural, textComponent: Text } = useIntl();
	const { value, other, children } = props;
	const pluralCategory = formatPlural(value, props);
	const formattedPlural = props[pluralCategory] || other;
	if (typeof children === "function") {
		return children(formattedPlural);
	}
	if (Text) {
		return /* @__PURE__ */ _jsx(Text, { children: formattedPlural });
	}
	// Work around @types/react where React.FC cannot return string
	return formattedPlural;
};
FormattedPlural.displayName = "FormattedPlural";
export default FormattedPlural;
