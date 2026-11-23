// @returns "ok"

export function run() {
  if (crypto !== globalThis.crypto || Object.prototype.toString.call(crypto) !== "[object Crypto]") {
    throw "Crypto global";
  }
  const backing = new Uint8Array(7).fill(255);
  const view = backing.subarray(2, 5);
  if (crypto.getRandomValues(view) !== view || backing.toHex() !== "ffff000102ffff") {
    throw "view identity or byte extent";
  }
  const types = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array,
    Int32Array, Uint32Array, BigInt64Array, BigUint64Array];
  for (const Type of types) {
    const view = new Type(32);
    crypto.getRandomValues(view);
    const bytes = new Uint8Array(view.buffer);
    for (let i = 0; i < bytes.length; i++) {
      if (bytes[i] !== i) throw "raw byte mismatch: " + Type.name;
    }
  }
  const empty = new Uint8Array(0);
  if (crypto.getRandomValues(empty) !== empty) throw "empty view identity";
  return "ok";
}
