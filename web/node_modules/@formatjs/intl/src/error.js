import "./types.js";
export let IntlErrorCode = /* @__PURE__ */ function(IntlErrorCode) {
	IntlErrorCode["FORMAT_ERROR"] = "FORMAT_ERROR";
	IntlErrorCode["UNSUPPORTED_FORMATTER"] = "UNSUPPORTED_FORMATTER";
	IntlErrorCode["INVALID_CONFIG"] = "INVALID_CONFIG";
	IntlErrorCode["MISSING_DATA"] = "MISSING_DATA";
	IntlErrorCode["MISSING_TRANSLATION"] = "MISSING_TRANSLATION";
	return IntlErrorCode;
}({});
export class IntlError extends Error {
	code;
	constructor(code, message, exception) {
		const err = exception ? exception instanceof Error ? exception : new Error(String(exception)) : undefined;
		super(`[@formatjs/intl Error ${code}] ${message}
${err ? `\n${err.message}\n${err.stack}` : ""}`);
		this.code = code;
		// @ts-ignore just so we don't need to declare dep on @types/node
		if (typeof Error.captureStackTrace === "function") {
			// @ts-ignore just so we don't need to declare dep on @types/node
			Error.captureStackTrace(this, IntlError);
		}
	}
}
export class UnsupportedFormatterError extends IntlError {
	constructor(message, exception) {
		super(IntlErrorCode.UNSUPPORTED_FORMATTER, message, exception);
	}
}
export class InvalidConfigError extends IntlError {
	constructor(message, exception) {
		super(IntlErrorCode.INVALID_CONFIG, message, exception);
	}
}
export class MissingDataError extends IntlError {
	constructor(message, exception) {
		super(IntlErrorCode.MISSING_DATA, message, exception);
	}
}
export class IntlFormatError extends IntlError {
	descriptor;
	locale;
	constructor(message, locale, exception) {
		super(IntlErrorCode.FORMAT_ERROR, `${message}
Locale: ${locale}
`, exception);
		this.locale = locale;
	}
}
export class MessageFormatError extends IntlFormatError {
	descriptor;
	locale;
	constructor(message, locale, descriptor, exception) {
		super(`${message}
MessageID: ${descriptor?.id}
Default Message: ${descriptor?.defaultMessage}
Description: ${descriptor?.description}
`, locale, exception);
		this.descriptor = descriptor;
		this.locale = locale;
	}
}
export class MissingTranslationError extends IntlError {
	descriptor;
	constructor(descriptor, locale) {
		super(IntlErrorCode.MISSING_TRANSLATION, `Missing message: "${descriptor.id}" for locale "${locale}", using ${descriptor.defaultMessage ? `default message (${typeof descriptor.defaultMessage === "string" ? descriptor.defaultMessage : descriptor.defaultMessage.map((e) => e.value ?? JSON.stringify(e)).join()})` : "id"} as fallback.`);
		this.descriptor = descriptor;
	}
}
