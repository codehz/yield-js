// @pending ["wait:decoder"]
// @restore
// @fulfill ["wait:decoder", 0]
// @fulfilled "😀|A|b"

import { wait } from 'test';

export async function run() {
  const bytes = new Uint8Array(10);
  bytes[3] = 0xef;
  bytes[4] = 0xbb;
  const view = new DataView(bytes.buffer, 3, 2);
  const decoder = new TextDecoder();
  if (decoder.decode(view, { stream: true }) !== "") {
    throw "streaming prefix must stay buffered";
  }

  const fatal = new TextDecoder("utf-8", { fatal: true });
  let failed = false;
  try {
    fatal.decode(Uint8Array.from([0x41, 0xff, 0x62]), { stream: true });
  } catch (error) {
    failed = error instanceof TypeError;
  }
  if (!failed) {
    throw "fatal stream error must throw a TypeError";
  }

  await wait("decoder");

  if (decoder.decode(Uint8Array.from([0xbf, 0xf0, 0x9f]), { stream: true }) !== "") {
    throw "BOM must be stripped across a restore and the new prefix buffered";
  }
  const emoji = decoder.decode(Uint8Array.from([0x98, 0x80]));
  if (emoji !== "😀") {
    throw "incomplete sequence did not resume: " + emoji;
  }

  const second = decoder.decode(Uint8Array.from([0xef, 0xbb, 0xbf, 0x41]));
  if (decoder.decode() !== "") {
    throw "empty final chunk must be empty";
  }
  const retained = fatal.decode();
  return emoji + "|" + second + "|" + retained;
}
