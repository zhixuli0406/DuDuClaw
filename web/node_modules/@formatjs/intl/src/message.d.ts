import { type CustomFormats, type Formatters, type MessageDescriptor, type OnErrorFn } from "./types.js";
import { type MessageFormatElement } from "@formatjs/icu-messageformat-parser";
import { type FormatXMLElementFn, type Formatters as IntlMessageFormatFormatters, type Options, type PrimitiveType } from "intl-messageformat";
export type FormatMessageFn<T> = ({ locale, formats, messages, defaultLocale, defaultFormats, fallbackOnEmptyString, onError, timeZone, defaultRichTextElements }: {
	locale: string;
	timeZone?: string;
	formats: CustomFormats;
	messages: Record<string, string> | Record<string, MessageFormatElement[]>;
	defaultLocale: string;
	defaultFormats: CustomFormats;
	defaultRichTextElements?: Record<string, FormatXMLElementFn<T>>;
	fallbackOnEmptyString?: boolean;
	onError: OnErrorFn;
}, state: IntlMessageFormatFormatters & Pick<Formatters, "getMessageFormat">, messageDescriptor: MessageDescriptor, values?: Record<string, PrimitiveType | T | FormatXMLElementFn<T>>, opts?: Options) => T extends string ? string : Array<T | string> | string | T;
export declare const formatMessage: FormatMessageFn<any>;
