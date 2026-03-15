import { type NumberFormatDigitInternalSlots, type NumberFormatDigitOptions, type NumberFormatNotation } from "../types/number.js";
/**
* https://tc39.es/ecma402/#sec-setnfdigitoptions
*/
export declare function SetNumberFormatDigitOptions(internalSlots: NumberFormatDigitInternalSlots, opts: NumberFormatDigitOptions, mnfdDefault: number, mxfdDefault: number, notation: NumberFormatNotation): void;
