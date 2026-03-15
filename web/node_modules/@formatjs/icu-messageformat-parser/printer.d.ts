import { type DateTimeSkeleton, type MessageFormatElement } from "./types.js";
export declare function printAST(ast: MessageFormatElement[]): string;
export declare function doPrintAST(ast: MessageFormatElement[], isInPlural: boolean): string;
export declare function printDateTimeSkeleton(style: DateTimeSkeleton): string;
