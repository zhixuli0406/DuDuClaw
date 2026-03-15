import { ArrayCreate } from "../262.js";
import "../types/number.js";
import { PartitionNumberPattern } from "./PartitionNumberPattern.js";
export function FormatNumericToParts(nf, x, implDetails) {
	const parts = PartitionNumberPattern(implDetails.getInternalSlots(nf), x);
	const result = ArrayCreate(0);
	for (const part of parts) {
		result.push({
			type: part.type,
			value: part.value
		});
	}
	return result;
}
