// @returns "ok"

export function run() {
  const buffer = new ArrayBuffer(8, { maxByteLength: 16 });
  if (buffer.resizable !== true || buffer.maxByteLength !== 16 || buffer.byteLength !== 8) {
    throw "resizable ArrayBuffer shape";
  }
  if (buffer.detached !== false) {
    throw "fresh buffer is not detached";
  }
  // A view without an explicit length tracks a resizable buffer; passing one
  // makes it fixed and lets a resize push it out of bounds.
  const fixed = new Uint8Array(buffer, 0, 8);
  const fixedView = new DataView(buffer, 0, 8);
  // Captured while the buffer still holds it; the resize below pushes it out.
  const pushedOut = new Uint16Array(buffer, 4, 2);
  const tracking = new Uint8Array(buffer, 4);
  const lateView = new Uint8Array(buffer, 4, 2);
  fixed[0] = 1;
  fixed[7] = 9;
  if (tracking.length !== 4) {
    throw "tracking view length";
  }
  buffer.resize(2);
  if (buffer.byteLength !== 2) {
    throw "shrink";
  }
  if (fixed.length !== 0 || fixed.byteLength !== 0) {
    throw "fixed view pushed out of bounds reports zero";
  }
  if (tracking.length !== 0) {
    throw "tracking view after shrink";
  }
  let outOfBounds = false;
  try {
    fixedView.byteLength;
  } catch (error) {
    outOfBounds = error instanceof TypeError;
  }
  if (!outOfBounds) {
    throw "an out-of-bounds DataView is a TypeError";
  }
  let offsetThrew = false;
  try {
    fixedView.byteOffset;
  } catch (error) {
    offsetThrew = error instanceof TypeError;
  }
  if (!offsetThrew || fixedView.buffer !== buffer) {
    throw "an out-of-bounds DataView reports no offset but keeps its buffer";
  }
  if (
    pushedOut.length !== 0 ||
    pushedOut.byteLength !== 0 ||
    pushedOut.byteOffset !== 0 ||
    pushedOut.buffer !== buffer
  ) {
    throw "an out-of-bounds TypedArray reports zero length, bytes, and offset";
  }
  buffer.resize(8);
  if (fixed.length !== 8) {
    throw "fixed view restored";
  }
  if (fixed[0] !== 1 || fixed[7] !== 0) {
    throw "truncated bytes must be zero after regrowth";
  }
  lateView[0] = 5;
  buffer.resize(2);
  if (lateView.length !== 0) {
    throw "view whose start is beyond the new length is out of bounds";
  }
  buffer.resize(8);
  if (lateView.length !== 2 || lateView[0] !== 0) {
    throw "released bytes are zero";
  }

  const empty = new ArrayBuffer(0);
  if (new DataView(empty).byteLength !== 0 || new Uint8Array(empty).length !== 0) {
    throw "zero-length buffer";
  }
  let rejected = false;
  try {
    new Uint8Array(empty, 1);
  } catch (error) {
    rejected = error instanceof RangeError;
  }
  if (!rejected) {
    throw "offset past the end is a RangeError";
  }

  const original = new ArrayBuffer(4, { maxByteLength: 8 });
  const view = new Uint8Array(original);
  view[0] = 7;
  const moved = original.transferToFixedLength();
  if (original.detached !== true || original.byteLength !== 0) {
    throw "transferToFixedLength detaches the source";
  }
  if (moved.byteLength !== 4 || moved.resizable !== false || moved.maxByteLength !== 4) {
    throw "transferToFixedLength shape";
  }
  if (new Uint8Array(moved)[0] !== 7) {
    throw "transferToFixedLength copies bytes";
  }
  if (view.length !== 0 || view.byteLength !== 0) {
    throw "views of a detached buffer report zero";
  }
  let detachedError = false;
  try {
    original.resize(8);
  } catch (error) {
    detachedError = error instanceof TypeError;
  }
  if (!detachedError) {
    throw "resizing a detached buffer is a TypeError";
  }

  const resizable = new ArrayBuffer(4, { maxByteLength: 8 });
  const copy = resizable.transfer();
  if (copy.resizable !== true || copy.maxByteLength !== 8 || copy.byteLength !== 4) {
    throw "transfer preserves resizability";
  }

  const victim = new ArrayBuffer(4);
  const victimView = new DataView(victim);
  let converted = false;
  const sneaky = {
    valueOf() {
      converted = true;
      victim.transfer();
      return 5;
    },
  };
  let stale = false;
  try {
    victimView.setUint8(0, sneaky);
  } catch (error) {
    stale = error instanceof TypeError;
  }
  if (!converted || !stale) {
    throw "conversion must run but no byte may be written after a transfer";
  }

  const growable = new ArrayBuffer(2, { maxByteLength: 4 });
  growable.resize(4);
  if (new Uint8Array(growable)[3] !== 0) {
    throw "grown tail is zero";
  }
  let rangeError = false;
  try {
    growable.resize(5);
  } catch (error) {
    rangeError = error instanceof RangeError;
  }
  if (!rangeError) {
    throw "resize past maxByteLength is a RangeError";
  }
  const sliced = new ArrayBuffer(8, { maxByteLength: 16 }).slice(1, 3);
  if (sliced.byteLength !== 2 || sliced.resizable !== false || sliced.maxByteLength !== 2) {
    throw "slice always produces a fixed-length buffer";
  }
  const growing = new ArrayBuffer(4, { maxByteLength: 8 });
  const bigger = growing.transfer(6);
  if (
    bigger.byteLength !== 6 ||
    bigger.resizable !== true ||
    bigger.maxByteLength !== 8 ||
    growing.detached !== true
  ) {
    throw "transfer keeps the old maximum byte length";
  }
  const overflowing = new ArrayBuffer(4, { maxByteLength: 8 });
  let transferRange = false;
  try {
    overflowing.transfer(9);
  } catch (error) {
    transferRange = error instanceof RangeError;
  }
  if (!transferRange || overflowing.detached !== false) {
    throw "transfer past maxByteLength is a RangeError and leaves the source attached";
  }
  let fixedResize = false;
  try {
    new ArrayBuffer(4).resize(2);
  } catch (error) {
    fixedResize = error instanceof TypeError;
  }
  if (!fixedResize) {
    throw "resizing a fixed-length ArrayBuffer is a TypeError";
  }
  const gone = new ArrayBuffer(2);
  gone.transfer();
  let detachedSlice = false;
  try {
    gone.slice(0, 1);
  } catch (error) {
    detachedSlice = error instanceof TypeError;
  }
  if (!detachedSlice) {
    throw "slicing a detached buffer is a TypeError";
  }
  if (
    ArrayBuffer.isView(new DataView(new ArrayBuffer(1))) !== true ||
    ArrayBuffer.isView(new Uint8Array(1)) !== true ||
    ArrayBuffer.isView({}) !== false
  ) {
    throw "ArrayBuffer.isView";
  }
  if (Object.prototype.toString.call(new DataView(new ArrayBuffer(1))) !== "[object DataView]") {
    throw "DataView toStringTag";
  }
  if (Object.prototype.toString.call(new ArrayBuffer(1)) !== "[object ArrayBuffer]") {
    throw "ArrayBuffer toStringTag";
  }
  let huge = 0;
  try {
    new Uint8Array(2 ** 53);
  } catch (error) {
    if (error instanceof RangeError) huge += 1;
  }
  try {
    new ArrayBuffer(2 ** 53);
  } catch (error) {
    if (error instanceof RangeError) huge += 1;
  }
  if (huge !== 2) {
    throw "lengths beyond 2^53-1 are RangeErrors";
  }
  return "ok";
}
