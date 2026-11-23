// @returns "ok"

function expectError(name, call) {
  try {
    call();
  } catch (e) {
    if (e instanceof DOMException && e.name === name) return;
    throw e;
  }
  throw "expected " + name;
}

export function run() {
  const invalid = [null, 1, {}, [], new ArrayBuffer(8), new DataView(new ArrayBuffer(8)),
    new Float16Array(1), new Float32Array(1), new Float64Array(1),
    new Proxy(new Uint8Array(1), {})];
  expectError("TypeMismatchError", () => crypto.getRandomValues());
  for (const value of invalid) {
    expectError("TypeMismatchError", () => crypto.getRandomValues(value));
  }
  expectError("QuotaExceededError", () => crypto.getRandomValues(new Uint8Array(65537)));
  expectError("QuotaExceededError", () => crypto.getRandomValues(new Uint16Array(32769)));
  const limit = new Uint16Array(32768);
  crypto.getRandomValues(limit);
  if (new Uint8Array(limit.buffer)[65535] !== 255) throw "quota boundary";

  const detached = new Uint8Array(4);
  detached.buffer.transfer();
  const resizable = new ArrayBuffer(8, { maxByteLength: 16 });
  const outOfBounds = new Uint8Array(resizable, 4, 4);
  resizable.resize(4);
  for (const view of [detached, outOfBounds]) {
    try {
      crypto.getRandomValues(view);
    } catch (e) {
      if (!(e instanceof TypeError)) throw e;
      continue;
    }
    throw "expected an invalid view to throw";
  }
  return "ok";
}
