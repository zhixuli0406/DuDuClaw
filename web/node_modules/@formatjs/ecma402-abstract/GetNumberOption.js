/**
* https://tc39.es/ecma402/#sec-getnumberoption
* @param options
* @param property
* @param min
* @param max
* @param fallback
*/
import { DefaultNumberOption } from "./DefaultNumberOption.js";
export function GetNumberOption(options, property, minimum, maximum, fallback) {
	const val = options[property];
	return DefaultNumberOption(val, minimum, maximum, fallback);
}
