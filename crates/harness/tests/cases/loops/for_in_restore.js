// @pending ["wait:own"]
// @restore
// @fulfill ["wait:own", "O"]
// @pending ["wait:proto"]
// @restore
// @fulfill ["wait:proto", "P"]
// @pending ["wait:string"]
// @restore
// @fulfill ["wait:string", "S"]
// @fulfilled "seen|Otail|firstProto|PlastProto|;0|S1|3|stringKey|lateStringKey|;0|3|stringKey|lateStringKey|;numberKey|;booleanKey|;symbolKey|"

import { wait } from 'test';

export async function run() {
  const parent = { firstProto: true, seen: true, removed: true, lastProto: true };
  const object = Object.create(parent);
  object.seen = true;
  object.tail = true;

  let trace = "";
  for (const key in object) {
    trace += key + "|";
    if (key === "seen" && trace === "seen|") {
      trace += await wait("own");
      delete object.seen; // The inherited copy must still count as visited.
      object.late = true;
    }
    if (key === "firstProto") {
      trace += await wait("proto");
      delete parent.removed; // The cursor must still refer to this prototype.
      parent.late = true;
    }
  }

  Object.prototype.length = true;
  String.prototype[0] = true;
  String.prototype[3] = true;
  String.prototype.stringKey = true;
  Number.prototype.numberKey = true;
  Boolean.prototype.booleanKey = true;
  Symbol.prototype.symbolKey = true;

  trace += ";";
  for (const key in "😀") {
    trace += key + "|";
    if (key === "0") {
      trace += await wait("string");
      String.prototype.lateStringKey = true;
    }
  }
  trace += ";";
  for (const key in "") trace += key + "|";
  delete Object.prototype.length;

  for (const primitive of [1, false, Symbol("primitive")]) {
    trace += ";";
    for (const key in primitive) trace += key + "|";
  }
  return trace;
}
