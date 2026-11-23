// @pending ["wait:codec-checkpoint"]
// @restore
// @fulfill ["wait:codec-checkpoint", 0]
// @fulfilled "ok"

import { wait } from 'test';

function eq(actual, expected, label) {
  if (actual !== expected) throw label + ': got ' + actual + ', expected ' + expected;
}

export async function run() {
  const utf8 = new TextDecoder(' UTF8 ', { fatal: true });
  const utf16 = new TextDecoder('unicodefffe');
  eq(utf8.decode(Uint8Array.of(0xef, 0xbb), { stream: true }), '', 'UTF-8 BOM prefix');
  eq(utf16.decode(Uint8Array.of(0xfe, 0xff, 0xd8, 0x3d, 0xde), { stream: true }), '', 'UTF-16 pair prefix');

  await wait('codec-checkpoint');

  // The nested call completes the restored BOM. The outer call must use the
  // new checkpoint, discard its fatal output, and retain the unread suffix.
  let error;
  try {
    utf8.decode(Uint8Array.of(0x41, 0xff, 0xef, 0xbb, 0xbf), {
      get stream() {
        eq(utf8.decode(Uint8Array.of(0xbf), { stream: true }), '', 'nested BOM completion');
        return true;
      }
    });
  } catch (caught) {
    error = caught;
  }
  eq(error instanceof TypeError, true, 'fatal chunk');
  eq(utf8.decode(), '\uFEFF', 'retained suffix with previously seen BOM');
  eq(utf8.decode(Uint8Array.of(0xef, 0xbb, 0xbf, 0x42)), 'B', 'BOM reset after flush');
  eq(utf16.decode(Uint8Array.of(0x00)), '😀', 'restored UTF-16 pair');

  const encoder = new TextEncoder();
  const text = '\ud800😀\udc00';
  const bytes = encoder.encode(text);
  eq(new TextDecoder().decode(bytes), '�😀�', 'one-shot UTF-16 scalar conversion');
  const target = new Uint8Array(6);
  const result = encoder.encodeInto(text, target);
  eq(result.read, 1, 'encodeInto does not split a scalar');
  eq(result.written, 3, 'encodeInto replacement width');
  eq(new TextDecoder().decode(target.subarray(0, result.written)), '�', 'encodeInto scalar conversion');
  return 'ok';
}
