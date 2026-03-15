import { HasOwnProperty } from "../262.js";
/**
* https://tc39.es/ecma402/#sec-currencydigits
*/
export function CurrencyDigits(c, { currencyDigitsData }) {
	return HasOwnProperty(currencyDigitsData, c) ? currencyDigitsData[c] : 2;
}
