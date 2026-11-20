// @pending ["wait:own"]
// @restore
// @fulfill ["wait:own", "O"]
// @pending ["wait:proto"]
// @restore
// @fulfill ["wait:proto", "P"]
// @fulfilled "seen|Otail|firstProto|PlastProto|"

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
  return trace;
}
