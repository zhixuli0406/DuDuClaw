export let ErrorCode = /* @__PURE__ */ function(ErrorCode) {
	// When we have a placeholder but no value to format
	ErrorCode["MISSING_VALUE"] = "MISSING_VALUE";
	// When value supplied is invalid
	ErrorCode["INVALID_VALUE"] = "INVALID_VALUE";
	// When we need specific Intl API but it's not available
	ErrorCode["MISSING_INTL_API"] = "MISSING_INTL_API";
	return ErrorCode;
}({});
export class FormatError extends Error {
	code;
	/**
	* Original message we're trying to format
	* `undefined` if we're only dealing w/ AST
	*
	* @type {(string | undefined)}
	* @memberof FormatError
	*/
	originalMessage;
	constructor(msg, code, originalMessage) {
		super(msg);
		this.code = code;
		this.originalMessage = originalMessage;
	}
	toString() {
		return `[formatjs Error: ${this.code}] ${this.message}`;
	}
}
export class InvalidValueError extends FormatError {
	constructor(variableId, value, options, originalMessage) {
		super(`Invalid values for "${variableId}": "${value}". Options are "${Object.keys(options).join("\", \"")}"`, ErrorCode.INVALID_VALUE, originalMessage);
	}
}
export class InvalidValueTypeError extends FormatError {
	constructor(value, type, originalMessage) {
		super(`Value for "${value}" must be of type ${type}`, ErrorCode.INVALID_VALUE, originalMessage);
	}
}
export class MissingValueError extends FormatError {
	constructor(variableId, originalMessage) {
		super(`The intl string context variable "${variableId}" was not provided to the string "${originalMessage}"`, ErrorCode.MISSING_VALUE, originalMessage);
	}
}
