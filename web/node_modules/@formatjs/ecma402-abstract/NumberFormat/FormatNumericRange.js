import "../types/number.js";
import { PartitionNumberRangePattern } from "./PartitionNumberRangePattern.js";
/**
* https://tc39.es/ecma402/#sec-formatnumericrange
*/
export function FormatNumericRange(numberFormat, x, y, { getInternalSlots }) {
	const parts = PartitionNumberRangePattern(numberFormat, x, y, { getInternalSlots });
	return parts.map((part) => part.value).join("");
}
