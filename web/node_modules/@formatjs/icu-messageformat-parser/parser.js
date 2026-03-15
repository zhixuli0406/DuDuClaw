import { ErrorKind } from "./error.js";
import { SKELETON_TYPE, TYPE } from "./types.js";
import { SPACE_SEPARATOR_REGEX } from "./regex.generated.js";
import { parseNumberSkeleton, parseNumberSkeletonFromString, parseDateTimeSkeleton } from "@formatjs/icu-skeleton-parser";
import { getBestPattern } from "./date-time-pattern-generator.js";
const SPACE_SEPARATOR_START_REGEX = new RegExp(`^${SPACE_SEPARATOR_REGEX.source}*`);
const SPACE_SEPARATOR_END_REGEX = new RegExp(`${SPACE_SEPARATOR_REGEX.source}*$`);
function createLocation(start, end) {
	return {
		start,
		end
	};
}
// #region Ponyfills
// Consolidate these variables up top for easier toggling during debugging
const hasNativeFromEntries = !!Object.fromEntries;
const hasTrimStart = !!String.prototype.trimStart;
const hasTrimEnd = !!String.prototype.trimEnd;
const fromEntries = hasNativeFromEntries ? Object.fromEntries : function fromEntries(entries) {
	const obj = {};
	for (const [k, v] of entries) {
		obj[k] = v;
	}
	return obj;
};
const trimStart = hasTrimStart ? function trimStart(s) {
	return s.trimStart();
} : function trimStart(s) {
	return s.replace(SPACE_SEPARATOR_START_REGEX, "");
};
const trimEnd = hasTrimEnd ? function trimEnd(s) {
	return s.trimEnd();
} : function trimEnd(s) {
	return s.replace(SPACE_SEPARATOR_END_REGEX, "");
};
// #endregion
const IDENTIFIER_PREFIX_RE = new RegExp("([^\\p{White_Space}\\p{Pattern_Syntax}]*)", "yu");
function matchIdentifierAtIndex(s, index) {
	IDENTIFIER_PREFIX_RE.lastIndex = index;
	const match = IDENTIFIER_PREFIX_RE.exec(s);
	return match[1] ?? "";
}
export class Parser {
	message;
	position;
	locale;
	ignoreTag;
	requiresOtherClause;
	shouldParseSkeletons;
	constructor(message, options = {}) {
		this.message = message;
		this.position = {
			offset: 0,
			line: 1,
			column: 1
		};
		this.ignoreTag = !!options.ignoreTag;
		this.locale = options.locale;
		this.requiresOtherClause = !!options.requiresOtherClause;
		this.shouldParseSkeletons = !!options.shouldParseSkeletons;
	}
	parse() {
		if (this.offset() !== 0) {
			throw Error("parser can only be used once");
		}
		return this.parseMessage(0, "", false);
	}
	parseMessage(nestingLevel, parentArgType, expectingCloseTag) {
		let elements = [];
		while (!this.isEOF()) {
			const char = this.char();
			if (char === 123) {
				const result = this.parseArgument(nestingLevel, expectingCloseTag);
				if (result.err) {
					return result;
				}
				elements.push(result.val);
			} else if (char === 125 && nestingLevel > 0) {
				break;
			} else if (char === 35 && (parentArgType === "plural" || parentArgType === "selectordinal")) {
				const position = this.clonePosition();
				this.bump();
				elements.push({
					type: TYPE.pound,
					location: createLocation(position, this.clonePosition())
				});
			} else if (char === 60 && !this.ignoreTag && this.peek() === 47) {
				if (expectingCloseTag) {
					break;
				} else {
					return this.error(ErrorKind.UNMATCHED_CLOSING_TAG, createLocation(this.clonePosition(), this.clonePosition()));
				}
			} else if (char === 60 && !this.ignoreTag && _isAlpha(this.peek() || 0)) {
				const result = this.parseTag(nestingLevel, parentArgType);
				if (result.err) {
					return result;
				}
				elements.push(result.val);
			} else {
				const result = this.parseLiteral(nestingLevel, parentArgType);
				if (result.err) {
					return result;
				}
				elements.push(result.val);
			}
		}
		return {
			val: elements,
			err: null
		};
	}
	/**
	* A tag name must start with an ASCII lower/upper case letter. The grammar is based on the
	* [custom element name][] except that a dash is NOT always mandatory and uppercase letters
	* are accepted:
	*
	* ```
	* tag ::= "<" tagName (whitespace)* "/>" | "<" tagName (whitespace)* ">" message "</" tagName (whitespace)* ">"
	* tagName ::= [a-z] (PENChar)*
	* PENChar ::=
	*     "-" | "." | [0-9] | "_" | [a-z] | [A-Z] | #xB7 | [#xC0-#xD6] | [#xD8-#xF6] | [#xF8-#x37D] |
	*     [#x37F-#x1FFF] | [#x200C-#x200D] | [#x203F-#x2040] | [#x2070-#x218F] | [#x2C00-#x2FEF] |
	*     [#x3001-#xD7FF] | [#xF900-#xFDCF] | [#xFDF0-#xFFFD] | [#x10000-#xEFFFF]
	* ```
	*
	* [custom element name]: https://html.spec.whatwg.org/multipage/custom-elements.html#valid-custom-element-name
	* NOTE: We're a bit more lax here since HTML technically does not allow uppercase HTML element but we do
	* since other tag-based engines like React allow it
	*/
	parseTag(nestingLevel, parentArgType) {
		const startPosition = this.clonePosition();
		this.bump();
		const tagName = this.parseTagName();
		this.bumpSpace();
		if (this.bumpIf("/>")) {
			// Self closing tag
			return {
				val: {
					type: TYPE.literal,
					value: `<${tagName}/>`,
					location: createLocation(startPosition, this.clonePosition())
				},
				err: null
			};
		} else if (this.bumpIf(">")) {
			const childrenResult = this.parseMessage(nestingLevel + 1, parentArgType, true);
			if (childrenResult.err) {
				return childrenResult;
			}
			const children = childrenResult.val;
			// Expecting a close tag
			const endTagStartPosition = this.clonePosition();
			if (this.bumpIf("</")) {
				if (this.isEOF() || !_isAlpha(this.char())) {
					return this.error(ErrorKind.INVALID_TAG, createLocation(endTagStartPosition, this.clonePosition()));
				}
				const closingTagNameStartPosition = this.clonePosition();
				const closingTagName = this.parseTagName();
				if (tagName !== closingTagName) {
					return this.error(ErrorKind.UNMATCHED_CLOSING_TAG, createLocation(closingTagNameStartPosition, this.clonePosition()));
				}
				this.bumpSpace();
				if (!this.bumpIf(">")) {
					return this.error(ErrorKind.INVALID_TAG, createLocation(endTagStartPosition, this.clonePosition()));
				}
				return {
					val: {
						type: TYPE.tag,
						value: tagName,
						children,
						location: createLocation(startPosition, this.clonePosition())
					},
					err: null
				};
			} else {
				return this.error(ErrorKind.UNCLOSED_TAG, createLocation(startPosition, this.clonePosition()));
			}
		} else {
			return this.error(ErrorKind.INVALID_TAG, createLocation(startPosition, this.clonePosition()));
		}
	}
	/**
	* This method assumes that the caller has peeked ahead for the first tag character.
	*/
	parseTagName() {
		const startOffset = this.offset();
		this.bump();
		while (!this.isEOF() && _isPotentialElementNameChar(this.char())) {
			this.bump();
		}
		return this.message.slice(startOffset, this.offset());
	}
	parseLiteral(nestingLevel, parentArgType) {
		const start = this.clonePosition();
		let value = "";
		while (true) {
			const parseQuoteResult = this.tryParseQuote(parentArgType);
			if (parseQuoteResult) {
				value += parseQuoteResult;
				continue;
			}
			const parseUnquotedResult = this.tryParseUnquoted(nestingLevel, parentArgType);
			if (parseUnquotedResult) {
				value += parseUnquotedResult;
				continue;
			}
			const parseLeftAngleResult = this.tryParseLeftAngleBracket();
			if (parseLeftAngleResult) {
				value += parseLeftAngleResult;
				continue;
			}
			break;
		}
		const location = createLocation(start, this.clonePosition());
		return {
			val: {
				type: TYPE.literal,
				value,
				location
			},
			err: null
		};
	}
	tryParseLeftAngleBracket() {
		if (!this.isEOF() && this.char() === 60 && (this.ignoreTag || !_isAlphaOrSlash(this.peek() || 0))) {
			this.bump();
			return "<";
		}
		return null;
	}
	/**
	* Starting with ICU 4.8, an ASCII apostrophe only starts quoted text if it immediately precedes
	* a character that requires quoting (that is, "only where needed"), and works the same in
	* nested messages as on the top level of the pattern. The new behavior is otherwise compatible.
	*/
	tryParseQuote(parentArgType) {
		if (this.isEOF() || this.char() !== 39) {
			return null;
		}
		// Parse escaped char following the apostrophe, or early return if there is no escaped char.
		// Check if is valid escaped character
		switch (this.peek()) {
			case 39:
				// double quote, should return as a single quote.
				this.bump();
				this.bump();
				return "'";
			case 123:
			case 60:
			case 62:
			case 125: break;
			case 35:
				if (parentArgType === "plural" || parentArgType === "selectordinal") {
					break;
				}
				return null;
			default: return null;
		}
		this.bump();
		const codePoints = [this.char()];
		this.bump();
		// read chars until the optional closing apostrophe is found
		while (!this.isEOF()) {
			const ch = this.char();
			if (ch === 39) {
				if (this.peek() === 39) {
					codePoints.push(39);
					// Bump one more time because we need to skip 2 characters.
					this.bump();
				} else {
					// Optional closing apostrophe.
					this.bump();
					break;
				}
			} else {
				codePoints.push(ch);
			}
			this.bump();
		}
		return String.fromCodePoint(...codePoints);
	}
	tryParseUnquoted(nestingLevel, parentArgType) {
		if (this.isEOF()) {
			return null;
		}
		const ch = this.char();
		if (ch === 60 || ch === 123 || ch === 35 && (parentArgType === "plural" || parentArgType === "selectordinal") || ch === 125 && nestingLevel > 0) {
			return null;
		} else {
			this.bump();
			return String.fromCodePoint(ch);
		}
	}
	parseArgument(nestingLevel, expectingCloseTag) {
		const openingBracePosition = this.clonePosition();
		this.bump();
		this.bumpSpace();
		if (this.isEOF()) {
			return this.error(ErrorKind.EXPECT_ARGUMENT_CLOSING_BRACE, createLocation(openingBracePosition, this.clonePosition()));
		}
		if (this.char() === 125) {
			this.bump();
			return this.error(ErrorKind.EMPTY_ARGUMENT, createLocation(openingBracePosition, this.clonePosition()));
		}
		// argument name
		let value = this.parseIdentifierIfPossible().value;
		if (!value) {
			return this.error(ErrorKind.MALFORMED_ARGUMENT, createLocation(openingBracePosition, this.clonePosition()));
		}
		this.bumpSpace();
		if (this.isEOF()) {
			return this.error(ErrorKind.EXPECT_ARGUMENT_CLOSING_BRACE, createLocation(openingBracePosition, this.clonePosition()));
		}
		switch (this.char()) {
			case 125: {
				this.bump();
				return {
					val: {
						type: TYPE.argument,
						value,
						location: createLocation(openingBracePosition, this.clonePosition())
					},
					err: null
				};
			}
			case 44: {
				this.bump();
				this.bumpSpace();
				if (this.isEOF()) {
					return this.error(ErrorKind.EXPECT_ARGUMENT_CLOSING_BRACE, createLocation(openingBracePosition, this.clonePosition()));
				}
				return this.parseArgumentOptions(nestingLevel, expectingCloseTag, value, openingBracePosition);
			}
			default: return this.error(ErrorKind.MALFORMED_ARGUMENT, createLocation(openingBracePosition, this.clonePosition()));
		}
	}
	/**
	* Advance the parser until the end of the identifier, if it is currently on
	* an identifier character. Return an empty string otherwise.
	*/
	parseIdentifierIfPossible() {
		const startingPosition = this.clonePosition();
		const startOffset = this.offset();
		const value = matchIdentifierAtIndex(this.message, startOffset);
		const endOffset = startOffset + value.length;
		this.bumpTo(endOffset);
		const endPosition = this.clonePosition();
		const location = createLocation(startingPosition, endPosition);
		return {
			value,
			location
		};
	}
	parseArgumentOptions(nestingLevel, expectingCloseTag, value, openingBracePosition) {
		// Parse this range:
		// {name, type, style}
		//        ^---^
		let typeStartPosition = this.clonePosition();
		let argType = this.parseIdentifierIfPossible().value;
		let typeEndPosition = this.clonePosition();
		switch (argType) {
			case "":
 // Expecting a style string number, date, time, plural, selectordinal, or select.
			return this.error(ErrorKind.EXPECT_ARGUMENT_TYPE, createLocation(typeStartPosition, typeEndPosition));
			case "number":
			case "date":
			case "time": {
				// Parse this range:
				// {name, number, style}
				//              ^-------^
				this.bumpSpace();
				let styleAndLocation = null;
				if (this.bumpIf(",")) {
					this.bumpSpace();
					const styleStartPosition = this.clonePosition();
					const result = this.parseSimpleArgStyleIfPossible();
					if (result.err) {
						return result;
					}
					const style = trimEnd(result.val);
					if (style.length === 0) {
						return this.error(ErrorKind.EXPECT_ARGUMENT_STYLE, createLocation(this.clonePosition(), this.clonePosition()));
					}
					const styleLocation = createLocation(styleStartPosition, this.clonePosition());
					styleAndLocation = {
						style,
						styleLocation
					};
				}
				const argCloseResult = this.tryParseArgumentClose(openingBracePosition);
				if (argCloseResult.err) {
					return argCloseResult;
				}
				const location = createLocation(openingBracePosition, this.clonePosition());
				// Extract style or skeleton
				if (styleAndLocation && styleAndLocation.style.startsWith("::")) {
					// Skeleton starts with `::`.
					let skeleton = trimStart(styleAndLocation.style.slice(2));
					if (argType === "number") {
						const result = this.parseNumberSkeletonFromString(skeleton, styleAndLocation.styleLocation);
						if (result.err) {
							return result;
						}
						return {
							val: {
								type: TYPE.number,
								value,
								location,
								style: result.val
							},
							err: null
						};
					} else {
						if (skeleton.length === 0) {
							return this.error(ErrorKind.EXPECT_DATE_TIME_SKELETON, location);
						}
						let dateTimePattern = skeleton;
						// Get "best match" pattern only if locale is passed, if not, let it
						// pass as-is where `parseDateTimeSkeleton()` will throw an error
						// for unsupported patterns.
						if (this.locale) {
							dateTimePattern = getBestPattern(skeleton, this.locale);
						}
						const style = {
							type: SKELETON_TYPE.dateTime,
							pattern: dateTimePattern,
							location: styleAndLocation.styleLocation,
							parsedOptions: this.shouldParseSkeletons ? parseDateTimeSkeleton(dateTimePattern) : {}
						};
						const type = argType === "date" ? TYPE.date : TYPE.time;
						return {
							val: {
								type,
								value,
								location,
								style
							},
							err: null
						};
					}
				}
				// Regular style or no style.
				return {
					val: {
						type: argType === "number" ? TYPE.number : argType === "date" ? TYPE.date : TYPE.time,
						value,
						location,
						style: styleAndLocation?.style ?? null
					},
					err: null
				};
			}
			case "plural":
			case "selectordinal":
			case "select": {
				// Parse this range:
				// {name, plural, options}
				//              ^---------^
				const typeEndPosition = this.clonePosition();
				this.bumpSpace();
				if (!this.bumpIf(",")) {
					return this.error(ErrorKind.EXPECT_SELECT_ARGUMENT_OPTIONS, createLocation(typeEndPosition, { ...typeEndPosition }));
				}
				this.bumpSpace();
				// Parse offset:
				// {name, plural, offset:1, options}
				//                ^-----^
				//
				// or the first option:
				//
				// {name, plural, one {...} other {...}}
				//                ^--^
				let identifierAndLocation = this.parseIdentifierIfPossible();
				let pluralOffset = 0;
				if (argType !== "select" && identifierAndLocation.value === "offset") {
					if (!this.bumpIf(":")) {
						return this.error(ErrorKind.EXPECT_PLURAL_ARGUMENT_OFFSET_VALUE, createLocation(this.clonePosition(), this.clonePosition()));
					}
					this.bumpSpace();
					const result = this.tryParseDecimalInteger(ErrorKind.EXPECT_PLURAL_ARGUMENT_OFFSET_VALUE, ErrorKind.INVALID_PLURAL_ARGUMENT_OFFSET_VALUE);
					if (result.err) {
						return result;
					}
					// Parse another identifier for option parsing
					this.bumpSpace();
					identifierAndLocation = this.parseIdentifierIfPossible();
					pluralOffset = result.val;
				}
				const optionsResult = this.tryParsePluralOrSelectOptions(nestingLevel, argType, expectingCloseTag, identifierAndLocation);
				if (optionsResult.err) {
					return optionsResult;
				}
				const argCloseResult = this.tryParseArgumentClose(openingBracePosition);
				if (argCloseResult.err) {
					return argCloseResult;
				}
				const location = createLocation(openingBracePosition, this.clonePosition());
				if (argType === "select") {
					return {
						val: {
							type: TYPE.select,
							value,
							options: fromEntries(optionsResult.val),
							location
						},
						err: null
					};
				} else {
					return {
						val: {
							type: TYPE.plural,
							value,
							options: fromEntries(optionsResult.val),
							offset: pluralOffset,
							pluralType: argType === "plural" ? "cardinal" : "ordinal",
							location
						},
						err: null
					};
				}
			}
			default: return this.error(ErrorKind.INVALID_ARGUMENT_TYPE, createLocation(typeStartPosition, typeEndPosition));
		}
	}
	tryParseArgumentClose(openingBracePosition) {
		// Parse: {value, number, ::currency/GBP }
		//
		if (this.isEOF() || this.char() !== 125) {
			return this.error(ErrorKind.EXPECT_ARGUMENT_CLOSING_BRACE, createLocation(openingBracePosition, this.clonePosition()));
		}
		this.bump();
		return {
			val: true,
			err: null
		};
	}
	/**
	* See: https://github.com/unicode-org/icu/blob/af7ed1f6d2298013dc303628438ec4abe1f16479/icu4c/source/common/messagepattern.cpp#L659
	*/
	parseSimpleArgStyleIfPossible() {
		let nestedBraces = 0;
		const startPosition = this.clonePosition();
		while (!this.isEOF()) {
			const ch = this.char();
			switch (ch) {
				case 39: {
					// Treat apostrophe as quoting but include it in the style part.
					// Find the end of the quoted literal text.
					this.bump();
					let apostrophePosition = this.clonePosition();
					if (!this.bumpUntil("'")) {
						return this.error(ErrorKind.UNCLOSED_QUOTE_IN_ARGUMENT_STYLE, createLocation(apostrophePosition, this.clonePosition()));
					}
					this.bump();
					break;
				}
				case 123: {
					nestedBraces += 1;
					this.bump();
					break;
				}
				case 125: {
					if (nestedBraces > 0) {
						nestedBraces -= 1;
					} else {
						return {
							val: this.message.slice(startPosition.offset, this.offset()),
							err: null
						};
					}
					break;
				}
				default:
					this.bump();
					break;
			}
		}
		return {
			val: this.message.slice(startPosition.offset, this.offset()),
			err: null
		};
	}
	parseNumberSkeletonFromString(skeleton, location) {
		let tokens = [];
		try {
			tokens = parseNumberSkeletonFromString(skeleton);
		} catch {
			return this.error(ErrorKind.INVALID_NUMBER_SKELETON, location);
		}
		return {
			val: {
				type: SKELETON_TYPE.number,
				tokens,
				location,
				parsedOptions: this.shouldParseSkeletons ? parseNumberSkeleton(tokens) : {}
			},
			err: null
		};
	}
	/**
	* @param nesting_level The current nesting level of messages.
	*     This can be positive when parsing message fragment in select or plural argument options.
	* @param parent_arg_type The parent argument's type.
	* @param parsed_first_identifier If provided, this is the first identifier-like selector of
	*     the argument. It is a by-product of a previous parsing attempt.
	* @param expecting_close_tag If true, this message is directly or indirectly nested inside
	*     between a pair of opening and closing tags. The nested message will not parse beyond
	*     the closing tag boundary.
	*/
	tryParsePluralOrSelectOptions(nestingLevel, parentArgType, expectCloseTag, parsedFirstIdentifier) {
		let hasOtherClause = false;
		const options = [];
		const parsedSelectors = new Set();
		let { value: selector, location: selectorLocation } = parsedFirstIdentifier;
		// Parse:
		// one {one apple}
		// ^--^
		while (true) {
			if (selector.length === 0) {
				const startPosition = this.clonePosition();
				if (parentArgType !== "select" && this.bumpIf("=")) {
					// Try parse `={number}` selector
					const result = this.tryParseDecimalInteger(ErrorKind.EXPECT_PLURAL_ARGUMENT_SELECTOR, ErrorKind.INVALID_PLURAL_ARGUMENT_SELECTOR);
					if (result.err) {
						return result;
					}
					selectorLocation = createLocation(startPosition, this.clonePosition());
					selector = this.message.slice(startPosition.offset, this.offset());
				} else {
					break;
				}
			}
			// Duplicate selector clauses
			if (parsedSelectors.has(selector)) {
				return this.error(parentArgType === "select" ? ErrorKind.DUPLICATE_SELECT_ARGUMENT_SELECTOR : ErrorKind.DUPLICATE_PLURAL_ARGUMENT_SELECTOR, selectorLocation);
			}
			if (selector === "other") {
				hasOtherClause = true;
			}
			// Parse:
			// one {one apple}
			//     ^----------^
			this.bumpSpace();
			const openingBracePosition = this.clonePosition();
			if (!this.bumpIf("{")) {
				return this.error(parentArgType === "select" ? ErrorKind.EXPECT_SELECT_ARGUMENT_SELECTOR_FRAGMENT : ErrorKind.EXPECT_PLURAL_ARGUMENT_SELECTOR_FRAGMENT, createLocation(this.clonePosition(), this.clonePosition()));
			}
			const fragmentResult = this.parseMessage(nestingLevel + 1, parentArgType, expectCloseTag);
			if (fragmentResult.err) {
				return fragmentResult;
			}
			const argCloseResult = this.tryParseArgumentClose(openingBracePosition);
			if (argCloseResult.err) {
				return argCloseResult;
			}
			options.push([selector, {
				value: fragmentResult.val,
				location: createLocation(openingBracePosition, this.clonePosition())
			}]);
			// Keep track of the existing selectors
			parsedSelectors.add(selector);
			// Prep next selector clause.
			this.bumpSpace();
			({value: selector, location: selectorLocation} = this.parseIdentifierIfPossible());
		}
		if (options.length === 0) {
			return this.error(parentArgType === "select" ? ErrorKind.EXPECT_SELECT_ARGUMENT_SELECTOR : ErrorKind.EXPECT_PLURAL_ARGUMENT_SELECTOR, createLocation(this.clonePosition(), this.clonePosition()));
		}
		if (this.requiresOtherClause && !hasOtherClause) {
			return this.error(ErrorKind.MISSING_OTHER_CLAUSE, createLocation(this.clonePosition(), this.clonePosition()));
		}
		return {
			val: options,
			err: null
		};
	}
	tryParseDecimalInteger(expectNumberError, invalidNumberError) {
		let sign = 1;
		const startingPosition = this.clonePosition();
		if (this.bumpIf("+")) {} else if (this.bumpIf("-")) {
			sign = -1;
		}
		let hasDigits = false;
		let decimal = 0;
		while (!this.isEOF()) {
			const ch = this.char();
			if (ch >= 48 && ch <= 57) {
				hasDigits = true;
				decimal = decimal * 10 + (ch - 48);
				this.bump();
			} else {
				break;
			}
		}
		const location = createLocation(startingPosition, this.clonePosition());
		if (!hasDigits) {
			return this.error(expectNumberError, location);
		}
		decimal *= sign;
		if (!Number.isSafeInteger(decimal)) {
			return this.error(invalidNumberError, location);
		}
		return {
			val: decimal,
			err: null
		};
	}
	offset() {
		return this.position.offset;
	}
	isEOF() {
		return this.offset() === this.message.length;
	}
	clonePosition() {
		// This is much faster than `Object.assign` or spread.
		return {
			offset: this.position.offset,
			line: this.position.line,
			column: this.position.column
		};
	}
	/**
	* Return the code point at the current position of the parser.
	* Throws if the index is out of bound.
	*/
	char() {
		const offset = this.position.offset;
		if (offset >= this.message.length) {
			throw Error("out of bound");
		}
		const code = this.message.codePointAt(offset);
		if (code === undefined) {
			throw Error(`Offset ${offset} is at invalid UTF-16 code unit boundary`);
		}
		return code;
	}
	error(kind, location) {
		return {
			val: null,
			err: {
				kind,
				message: this.message,
				location
			}
		};
	}
	/** Bump the parser to the next UTF-16 code unit. */
	bump() {
		if (this.isEOF()) {
			return;
		}
		const code = this.char();
		if (code === 10) {
			this.position.line += 1;
			this.position.column = 1;
			this.position.offset += 1;
		} else {
			this.position.column += 1;
			// 0 ~ 0x10000 -> unicode BMP, otherwise skip the surrogate pair.
			this.position.offset += code < 65536 ? 1 : 2;
		}
	}
	/**
	* If the substring starting at the current position of the parser has
	* the given prefix, then bump the parser to the character immediately
	* following the prefix and return true. Otherwise, don't bump the parser
	* and return false.
	*/
	bumpIf(prefix) {
		if (this.message.startsWith(prefix, this.offset())) {
			for (let i = 0; i < prefix.length; i++) {
				this.bump();
			}
			return true;
		}
		return false;
	}
	/**
	* Bump the parser until the pattern character is found and return `true`.
	* Otherwise bump to the end of the file and return `false`.
	*/
	bumpUntil(pattern) {
		const currentOffset = this.offset();
		const index = this.message.indexOf(pattern, currentOffset);
		if (index >= 0) {
			this.bumpTo(index);
			return true;
		} else {
			this.bumpTo(this.message.length);
			return false;
		}
	}
	/**
	* Bump the parser to the target offset.
	* If target offset is beyond the end of the input, bump the parser to the end of the input.
	*/
	bumpTo(targetOffset) {
		if (this.offset() > targetOffset) {
			throw Error(`targetOffset ${targetOffset} must be greater than or equal to the current offset ${this.offset()}`);
		}
		targetOffset = Math.min(targetOffset, this.message.length);
		while (true) {
			const offset = this.offset();
			if (offset === targetOffset) {
				break;
			}
			if (offset > targetOffset) {
				throw Error(`targetOffset ${targetOffset} is at invalid UTF-16 code unit boundary`);
			}
			this.bump();
			if (this.isEOF()) {
				break;
			}
		}
	}
	/** advance the parser through all whitespace to the next non-whitespace code unit. */
	bumpSpace() {
		while (!this.isEOF() && _isWhiteSpace(this.char())) {
			this.bump();
		}
	}
	/**
	* Peek at the *next* Unicode codepoint in the input without advancing the parser.
	* If the input has been exhausted, then this returns null.
	*/
	peek() {
		if (this.isEOF()) {
			return null;
		}
		const code = this.char();
		const offset = this.offset();
		const nextCode = this.message.charCodeAt(offset + (code >= 65536 ? 2 : 1));
		return nextCode ?? null;
	}
}
/**
* This check if codepoint is alphabet (lower & uppercase)
* @param codepoint
* @returns
*/
function _isAlpha(codepoint) {
	return codepoint >= 97 && codepoint <= 122 || codepoint >= 65 && codepoint <= 90;
}
function _isAlphaOrSlash(codepoint) {
	return _isAlpha(codepoint) || codepoint === 47;
}
/** See `parseTag` function docs. */
function _isPotentialElementNameChar(c) {
	return c === 45 || c === 46 || c >= 48 && c <= 57 || c === 95 || c >= 97 && c <= 122 || c >= 65 && c <= 90 || c == 183 || c >= 192 && c <= 214 || c >= 216 && c <= 246 || c >= 248 && c <= 893 || c >= 895 && c <= 8191 || c >= 8204 && c <= 8205 || c >= 8255 && c <= 8256 || c >= 8304 && c <= 8591 || c >= 11264 && c <= 12271 || c >= 12289 && c <= 55295 || c >= 63744 && c <= 64975 || c >= 65008 && c <= 65533 || c >= 65536 && c <= 983039;
}
/**
* Code point equivalent of regex `\p{White_Space}`.
* From: https://www.unicode.org/Public/UCD/latest/ucd/PropList.txt
*/
function _isWhiteSpace(c) {
	return c >= 9 && c <= 13 || c === 32 || c === 133 || c >= 8206 && c <= 8207 || c === 8232 || c === 8233;
}
