/*
* Copyright 2015, Yahoo Inc.
* Copyrights licensed under the New BSD License.
* See the accompanying LICENSE file for terms.
*/
import * as React from "react";
import "@formatjs/intl";
import { invariant } from "../utils.js";
import useIntl from "./useIntl.js";
import { jsx as _jsx, Fragment as _Fragment } from "react/jsx-runtime";
const MINUTE = 60;
const HOUR = 60 * 60;
const DAY = 60 * 60 * 24;
function selectUnit(seconds) {
	const absValue = Math.abs(seconds);
	if (absValue < MINUTE) {
		return "second";
	}
	if (absValue < HOUR) {
		return "minute";
	}
	if (absValue < DAY) {
		return "hour";
	}
	return "day";
}
function getDurationInSeconds(unit) {
	switch (unit) {
		case "second": return 1;
		case "minute": return MINUTE;
		case "hour": return HOUR;
		default: return DAY;
	}
}
function valueToSeconds(value, unit) {
	if (!value) {
		return 0;
	}
	switch (unit) {
		case "second": return value;
		case "minute": return value * MINUTE;
		default: return value * HOUR;
	}
}
const INCREMENTABLE_UNITS = [
	"second",
	"minute",
	"hour"
];
function canIncrement(unit = "second") {
	return INCREMENTABLE_UNITS.indexOf(unit) > -1;
}
const SimpleFormattedRelativeTime = (props) => {
	const { formatRelativeTime, textComponent: Text } = useIntl();
	const { children, value, unit, ...otherProps } = props;
	const formattedRelativeTime = formatRelativeTime(value || 0, unit, otherProps);
	if (typeof children === "function") {
		return children(formattedRelativeTime);
	}
	if (Text) {
		return /* @__PURE__ */ _jsx(Text, { children: formattedRelativeTime });
	}
	return /* @__PURE__ */ _jsx(_Fragment, { children: formattedRelativeTime });
};
const FormattedRelativeTime = ({ value = 0, unit = "second", updateIntervalInSeconds, ...otherProps }) => {
	invariant(!updateIntervalInSeconds || !!(updateIntervalInSeconds && canIncrement(unit)), "Cannot schedule update with unit longer than hour");
	const [prevUnit, setPrevUnit] = React.useState();
	const [prevValue, setPrevValue] = React.useState(0);
	const [currentValueInSeconds, setCurrentValueInSeconds] = React.useState(0);
	const updateTimer = React.useRef(undefined);
	if (unit !== prevUnit || value !== prevValue) {
		setPrevValue(value || 0);
		setPrevUnit(unit);
		setCurrentValueInSeconds(canIncrement(unit) ? valueToSeconds(value, unit) : 0);
	}
	React.useEffect(() => {
		function clearUpdateTimer() {
			clearTimeout(updateTimer.current);
		}
		clearUpdateTimer();
		// If there's no interval and we cannot increment this unit, do nothing
		if (!updateIntervalInSeconds || !canIncrement(unit)) {
			return clearUpdateTimer;
		}
		// Figure out the next interesting time
		const nextValueInSeconds = currentValueInSeconds - updateIntervalInSeconds;
		const nextUnit = selectUnit(nextValueInSeconds);
		// We've reached the max auto incrementable unit, don't schedule another update
		if (nextUnit === "day") {
			return clearUpdateTimer;
		}
		const unitDuration = getDurationInSeconds(nextUnit);
		const remainder = nextValueInSeconds % unitDuration;
		const prevInterestingValueInSeconds = nextValueInSeconds - remainder;
		const nextInterestingValueInSeconds = prevInterestingValueInSeconds >= currentValueInSeconds ? prevInterestingValueInSeconds - unitDuration : prevInterestingValueInSeconds;
		const delayInSeconds = Math.abs(nextInterestingValueInSeconds - currentValueInSeconds);
		if (currentValueInSeconds !== nextInterestingValueInSeconds) {
			updateTimer.current = setTimeout(() => setCurrentValueInSeconds(nextInterestingValueInSeconds), delayInSeconds * 1e3);
		}
		return clearUpdateTimer;
	}, [
		currentValueInSeconds,
		updateIntervalInSeconds,
		unit
	]);
	let currentValue = value || 0;
	let currentUnit = unit;
	if (canIncrement(unit) && typeof currentValueInSeconds === "number" && updateIntervalInSeconds) {
		currentUnit = selectUnit(currentValueInSeconds);
		const unitDuration = getDurationInSeconds(currentUnit);
		currentValue = Math.round(currentValueInSeconds / unitDuration);
	}
	return /* @__PURE__ */ _jsx(SimpleFormattedRelativeTime, {
		value: currentValue,
		unit: currentUnit,
		...otherProps
	});
};
FormattedRelativeTime.displayName = "FormattedRelativeTime";
export default FormattedRelativeTime;
