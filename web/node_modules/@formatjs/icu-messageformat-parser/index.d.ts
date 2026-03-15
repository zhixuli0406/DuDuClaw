import { Parser, type ParserOptions } from "./parser.js";
import { type MessageFormatElement } from "./types.js";
export declare function parse(message: string, opts?: ParserOptions): MessageFormatElement[];
export * from "./types.js";
export type { ParserOptions };
export declare const _Parser: typeof Parser;
export { isStructurallySame } from "./manipulator.js";
