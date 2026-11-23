// @pending ["wait:strategy"]
// @restore
// @fulfill ["wait:strategy", true]
// @fulfilled true

// Upstream surface: interfaces/streams.idl (queuing strategy interfaces).
import { wait, gc } from "test";

export async function run() {
  let calls = 0;
  const count = new CountQueuingStrategy({
    get highWaterMark() { calls++; return Infinity; }
  });
  const bytes = new ByteLengthQueuingStrategy({ highWaterMark: NaN });
  const countSize = count.size;
  const byteSize = bytes.size;
  const reason = {};
  gc();
  await wait("strategy");
  if (calls !== 1 || count.highWaterMark !== Infinity || !Number.isNaN(bytes.highWaterMark)) return false;
  if (count.size !== countSize || bytes.size !== byteSize || countSize() !== 1) return false;
  if (count.size !== new globalThis.CountQueuingStrategy({ highWaterMark: 0 }).size) return false;
  const chunk = { get byteLength() { return this === chunk ? reason : null; } };
  if (byteSize(chunk) !== reason) return false;
  const descriptor = Object.getOwnPropertyDescriptor(ByteLengthQueuingStrategy.prototype, "size");
  if (!descriptor.enumerable || !descriptor.configurable || descriptor.set !== undefined) return false;
  try {
    descriptor.get.call(count);
  } catch (error) {
    return error instanceof TypeError;
  }
  return false;
}
