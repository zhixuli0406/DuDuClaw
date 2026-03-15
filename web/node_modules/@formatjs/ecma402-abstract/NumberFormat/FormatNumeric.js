import "../types/number.js";
import { PartitionNumberPattern } from "./PartitionNumberPattern.js";
export function FormatNumeric(internalSlots, x) {
	const parts = PartitionNumberPattern(internalSlots, x);
	return parts.map((p) => p.value).join("");
}
