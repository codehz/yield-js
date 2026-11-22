// @returns true

import { expect } from "test";

export function run() {
  const reserved = ";/?:@&=+$,#";
  const escaped = "%3b%2f%3F%3a%40%26%3D%2b%24%2C%23";
  expect(encodeURI(reserved)).toBe(reserved);
  expect(encodeURIComponent(reserved)).toBe("%3B%2F%3F%3A%40%26%3D%2B%24%2C%23");
  expect(decodeURI(escaped + "%41%25")).toBe(escaped + "A%");
  expect(decodeURIComponent(escaped + "%41%25")).toBe(reserved + "A%");

  const text = "a b&c/中😀é!'()*-._~";
  const encoded = "a%20b%26c%2F%E4%B8%AD%F0%9F%98%80%C3%A9!'()*-._~";
  expect(encodeURIComponent(text)).toBe(encoded);
  expect(decodeURIComponent(encoded)).toBe(text);
  expect(decodeURIComponent("\uD800%41\uDC00")).toBe("\uD800A\uDC00");
  expect(decodeURIComponent("%EF%BB%BF%00+")).toBe("\uFEFF\0+");

  const input = { [Symbol.toPrimitive](hint) { expect(hint).toBe("string"); return "%41"; } };
  expect(encodeURI(input)).toBe("%2541");
  expect(decodeURI(input)).toBe("A");
  expect(encodeURI()).toBe("undefined");
  for (const fn of [encodeURI, encodeURIComponent, decodeURI, decodeURIComponent]) {
    try { fn(Symbol()); return "symbol converted"; }
    catch (error) { expect(error instanceof TypeError).toBe(true); }
  }
  for (const fn of [encodeURI, encodeURIComponent]) {
    for (const bad of ["\uD800", "\uDC00", "\uD800x"]) {
      try { fn(bad); return "encoded lone surrogate"; }
      catch (error) { expect(error instanceof URIError).toBe(true); }
    }
  }
  for (const fn of [decodeURI, decodeURIComponent]) {
    for (const bad of ["%", "%0", "%ZZ", "%80", "%C0%AF", "%E0%80%AF", "%E4%B8", "%E4x%B8%AD",
                       "%E4%41%AD", "%ED%A0%80", "%F4%90%80%80", "%F5%80%80%80"]) {
      try { fn(bad); return "decoded malformed URI: " + bad; }
      catch (error) { expect(error instanceof URIError).toBe(true); }
    }
  }
  return true;
}
