// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 42]
// @fulfilled "ok"

import { wait } from "test";

export async function run() {
  const buffer = new ArrayBuffer(8);
  const bytes = new Uint8Array(buffer);
  const words = new Uint16Array(buffer, 0, 3);
  const data = new DataView(buffer, 2, 4);
  bytes[0] = 0x11;
  data.setUint16(0, 0x2233);
  const iterator = new Uint8Array([7, 8, 9]).values();
  const firstStep = iterator.next().value;
  const secondStep = iterator.next().value;
  const resizable = new ArrayBuffer(4, { maxByteLength: 8 });
  const tracking = new Uint8Array(resizable);
  tracking[0] = 5;
  if (firstStep !== 7 || secondStep !== 8) {
    throw "iterator before the suspend";
  }

  let stage = "resume";
  let answer;
  try {
    answer = await wait("a");
  } catch (error) {
    throw "await: " + (error && error.message);
  }
  try {
  stage = "host";
  if (answer !== 42) {
    throw "host value after restore";
  }
  stage = "bytes";
  if (words[0] !== 0x11) {
    throw "buffer bytes survive the restore";
  }
  stage = "dataview";
  if (bytes[2] !== 0x22 || bytes[3] !== 0x33) {
    throw "DataView writes survive the restore";
  }
  stage = "shared";
  if (data.getUint16(0) !== 0x2233 || words[1] !== 0x3322) {
    throw "every view still sees the same bytes";
  }
  stage = "alias";
  bytes[4] = 9;
  if (data.getUint8(2) !== 9) {
    throw "views still alias after the restore";
  }
  stage = "iter";
  if (iterator.next().value !== 9) {
    throw "iterator resumes at the third element";
  }
  stage = "iterDone";
  if (iterator.next().done !== true) {
    throw "iterator finishes";
  }
  stage = "meta";
  if (resizable.maxByteLength !== 8 || tracking.length !== 4) {
    throw "resizable buffer metadata";
  }
  stage = "trackBytes";
  if (tracking[0] !== 5) {
    throw "tracking view bytes";
  }
  stage = "grow";
  resizable.resize(8);
  if (tracking.length !== 8) {
    throw "an automatic-length view grows with the buffer";
  }
  stage = "shrink";
  resizable.resize(2);
  if (tracking.length !== 2) {
    throw "an automatic-length view shrinks with the buffer";
  }
  } catch (error) {
    throw stage + ": " + (typeof error === "string" ? error : (error && error.message) || "object");
  }
  return "ok";
}
