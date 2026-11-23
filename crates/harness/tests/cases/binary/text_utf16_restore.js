// @pending ["wait:utf16"]
// @restore
// @fulfill ["wait:utf16", 0]
// @fulfilled "ok"

import { wait } from 'test';

function eq(actual, expected, message) {
  if (actual !== expected) throw message + ': got ' + actual + ', expected ' + expected;
}

function decode(decoder, bytes, stream = false) {
  return decoder.decode(Uint8Array.from(bytes), { stream });
}

export async function run() {
  for (const label of ["csunicode", "iso-10646-ucs-2", "ucs-2", "unicode", "unicodefeff", "utf-16", "utf-16le"]) {
    eq(new TextDecoder(label).encoding, "utf-16le", label);
  }
  const le = new TextDecoder(" \tUtF-16\r\n");
  const be = new TextDecoder("UnicodeFFFE");
  eq(be.encoding, "utf-16be", "canonical BE label");
  eq(decode(le, [0xff], true), "", "split LE BOM");
  eq(decode(be, [0xfe, 0xff, 0xd8, 0x3d, 0xde], true), "", "BE BOM and incomplete pair");

  const fatal = new TextDecoder("utf-16le", { fatal: true });
  let failure;
  try {
    decode(fatal, [0x00, 0xd8, 0x41, 0x00], true);
  } catch (error) {
    failure = error;
  }
  eq(failure instanceof TypeError, true, "fatal unpaired high surrogate");

  await wait("utf16");

  eq(decode(le, [0xfe, 0x41, 0x00]), "A", "resume split BOM");
  eq(decode(be, [0x00, 0xfe, 0xff]), "😀\uFEFF", "resume pair and preserve later BOM");
  eq(decode(be, [0xfe, 0xff, 0x00, 0x42]), "B", "BOM reset between streams");
  eq(fatal.decode(), "A", "reprocess retained unit after fatal error");
  eq(decode(le, [0x00, 0xd8, 0x41, 0x00, 0x00, 0xdc]), "�A�", "replace unpaired surrogates");
  eq(decode(le, [0x00, 0xd8, 0x41]), "�", "flush high surrogate plus odd byte as one error");
  eq(decode(le, [0xfe, 0xff]), "\uFFFE", "BOM does not switch endianness");
  eq(decode(new TextDecoder("utf-16be", { ignoreBOM: true }), [0xfe, 0xff]), "\uFEFF", "ignoreBOM preserves BOM");
  return "ok";
}
