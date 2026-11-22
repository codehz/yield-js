import { clearTimeout, setTimeout } from "test";
import * as console from "console";
export { clearTimeout, setTimeout, console };
export function reportError(error) {}
export const kEvents = Symbol("kEvents");

const uriHexDigits = "0123456789ABCDEF";
const uriReserved = ";/?:@&=+$,#";
const uriUnescaped = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.!~*'()";
const uriDecoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

function uriByte(byte) {
    return "%" + uriHexDigits[byte >> 4] + uriHexDigits[byte & 15];
}

function uriHexValue(code) {
    if (code >= 0x30 && code <= 0x39) return code - 0x30;
    if (code >= 0x41 && code <= 0x46) return code - 0x41 + 10;
    if (code >= 0x61 && code <= 0x66) return code - 0x61 + 10;
    return -1;
}

// One percent triplet starting at `at`, or -1 when the triplet is malformed.
function uriHex(text, at) {
    if (at + 2 >= text.length || text[at] !== "%") return -1;
    const hi = uriHexValue(text.charCodeAt(at + 1));
    const lo = uriHexValue(text.charCodeAt(at + 2));
    return hi < 0 || lo < 0 ? -1 : hi * 16 + lo;
}

// ToString plus UTF-8 percent encoding; `extra` is the code-unit set the
// caller leaves unescaped. Lone surrogates are URI errors, not U+FFFD.
function uriEncode(text, extra) {
    let out = "";
    for (let index = 0; index < text.length; index++) {
        const unit = text.charCodeAt(index);
        if (unit < 0x80 && extra.indexOf(text[index]) >= 0) {
            out += text[index];
            continue;
        }
        let point = unit;
        if (unit >= 0xd800 && unit <= 0xdbff) {
            const next = index + 1 < text.length ? text.charCodeAt(index + 1) : 0;
            if (next < 0xdc00 || next > 0xdfff) throw new URIError("URI malformed");
            point = (unit - 0xd800) * 0x400 + (next - 0xdc00) + 0x10000;
            index += 1;
        } else if (unit >= 0xdc00 && unit <= 0xdfff) {
            throw new URIError("URI malformed");
        }
        if (point < 0x80) {
            out += uriByte(point);
        } else if (point < 0x800) {
            out += uriByte(0xc0 | (point >> 6));
            out += uriByte(0x80 | (point & 0x3f));
        } else if (point < 0x10000) {
            out += uriByte(0xe0 | (point >> 12));
            out += uriByte(0x80 | ((point >> 6) & 0x3f));
            out += uriByte(0x80 | (point & 0x3f));
        } else {
            out += uriByte(0xf0 | (point >> 18));
            out += uriByte(0x80 | ((point >> 12) & 0x3f));
            out += uriByte(0x80 | ((point >> 6) & 0x3f));
            out += uriByte(0x80 | (point & 0x3f));
        }
    }
    return out;
}

// Percent decoding of one UTF-8 scalar at a time. A fatal decoder rejects
// overlong, surrogate and out-of-range sequences as URI errors. When
// `reserved` is set (decodeURI), reserved characters keep their original
// percent spelling.
function uriDecode(text, reserved) {
    let out = "";
    for (let index = 0; index < text.length; index++) {
        if (text[index] !== "%") {
            out += text[index];
            continue;
        }
        const first = uriHex(text, index);
        if (first < 0) throw new URIError("URI malformed");
        let count;
        if (first < 0x80) count = 1;
        else if (first >= 0xc2 && first <= 0xdf) count = 2;
        else if (first >= 0xe0 && first <= 0xef) count = 3;
        else if (first >= 0xf0 && first <= 0xf4) count = 4;
        else throw new URIError("URI malformed");
        const bytes = [first];
        let cursor = index + 3;
        for (let offset = 1; offset < count; offset++) {
            const byte = uriHex(text, cursor);
            if (byte < 0) throw new URIError("URI malformed");
            bytes.push(byte);
            cursor += 3;
        }
        let decoded;
        try {
            decoded = uriDecoder.decode(new Uint8Array(bytes));
        } catch (error) {
            throw new URIError("URI malformed");
        }
        if (reserved && uriReserved.indexOf(decoded) >= 0) {
            out += text.slice(index, cursor);
        } else {
            out += decoded;
        }
        index = cursor - 1;
    }
    return out;
}

export function encodeURIComponent(value) {
    return uriEncode("" + value, uriUnescaped);
}

export function encodeURI(value) {
    return uriEncode("" + value, uriUnescaped + uriReserved);
}

export function decodeURIComponent(value) {
    return uriDecode("" + value, false);
}

export function decodeURI(value) {
    return uriDecode("" + value, true);
}
