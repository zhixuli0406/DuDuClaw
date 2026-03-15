import "./types.js";
import { TYPE } from "@formatjs/icu-messageformat-parser";
import { IntlMessageFormat } from "intl-messageformat";
import { MessageFormatError, MissingTranslationError } from "./error.js";
import { invariant } from "./utils.js";
function setTimeZoneInOptions(opts, timeZone) {
	return Object.keys(opts).reduce((all, k) => {
		all[k] = {
			timeZone,
			...opts[k]
		};
		return all;
	}, {});
}
function deepMergeOptions(opts1, opts2) {
	const keys = Object.keys({
		...opts1,
		...opts2
	});
	return keys.reduce((all, k) => {
		all[k] = {
			...opts1[k],
			...opts2[k]
		};
		return all;
	}, {});
}
function deepMergeFormatsAndSetTimeZone(f1, timeZone) {
	if (!timeZone) {
		return f1;
	}
	const mfFormats = IntlMessageFormat.formats;
	return {
		...mfFormats,
		...f1,
		date: deepMergeOptions(setTimeZoneInOptions(mfFormats.date, timeZone), setTimeZoneInOptions(f1.date || {}, timeZone)),
		time: deepMergeOptions(setTimeZoneInOptions(mfFormats.time, timeZone), setTimeZoneInOptions(f1.time || {}, timeZone))
	};
}
export const formatMessage = ({ locale, formats, messages, defaultLocale, defaultFormats, fallbackOnEmptyString, onError, timeZone, defaultRichTextElements }, state, messageDescriptor = { id: "" }, values, opts) => {
	const { id: msgId, defaultMessage } = messageDescriptor;
	// `id` is a required field of a Message Descriptor.
	invariant(!!msgId, `[@formatjs/intl] An \`id\` must be provided to format a message. You can either:
1. Configure your build toolchain with [babel-plugin-formatjs](https://formatjs.github.io/docs/tooling/babel-plugin)
or [@formatjs/ts-transformer](https://formatjs.github.io/docs/tooling/ts-transformer) OR
2. Configure your \`eslint\` config to include [eslint-plugin-formatjs](https://formatjs.github.io/docs/tooling/linter#enforce-id)
to autofix this issue`);
	const id = String(msgId);
	const message = messages && Object.prototype.hasOwnProperty.call(messages, id) && messages[id];
	// IMPORTANT: Hot path if `message` is AST with a single literal node
	if (Array.isArray(message) && message.length === 1 && message[0].type === TYPE.literal) {
		return message[0].value;
	}
	values = {
		...defaultRichTextElements,
		...values
	};
	formats = deepMergeFormatsAndSetTimeZone(formats, timeZone);
	defaultFormats = deepMergeFormatsAndSetTimeZone(defaultFormats, timeZone);
	if (!message) {
		if (fallbackOnEmptyString === false && message === "") {
			return message;
		}
		if (!defaultMessage || locale && locale.toLowerCase() !== defaultLocale.toLowerCase()) {
			// This prevents warnings from littering the console in development
			// when no `messages` are passed into the <IntlProvider> for the
			// default locale.
			onError(new MissingTranslationError(messageDescriptor, locale));
		}
		if (defaultMessage) {
			try {
				const formatter = state.getMessageFormat(defaultMessage, defaultLocale, defaultFormats, opts);
				return formatter.format(values);
			} catch (e) {
				onError(new MessageFormatError(`Error formatting default message for: "${id}", rendering default message verbatim`, locale, messageDescriptor, e));
				return typeof defaultMessage === "string" ? defaultMessage : id;
			}
		}
		return id;
	}
	// We have the translated message
	try {
		const formatter = state.getMessageFormat(message, locale, formats, {
			formatters: state,
			...opts
		});
		return formatter.format(values);
	} catch (e) {
		onError(new MessageFormatError(`Error formatting message: "${id}", using ${defaultMessage ? "default message" : "id"} as fallback.`, locale, messageDescriptor, e));
	}
	if (defaultMessage) {
		try {
			const formatter = state.getMessageFormat(defaultMessage, defaultLocale, defaultFormats, opts);
			return formatter.format(values);
		} catch (e) {
			onError(new MessageFormatError(`Error formatting the default message for: "${id}", rendering message verbatim`, locale, messageDescriptor, e));
		}
	}
	if (typeof message === "string") {
		return message;
	}
	if (typeof defaultMessage === "string") {
		return defaultMessage;
	}
	return id;
};
