// @returns "ok"

export function run() {
  const clamped = new Uint8ClampedArray(4);
  clamped[0] = 2.5;
  clamped[1] = 3.5;
  clamped[2] = -1;
  clamped[3] = 300;
  if (clamped[0] !== 2 || clamped[1] !== 4 || clamped[2] !== 0 || clamped[3] !== 255) {
    throw "Uint8ClampedArray clamp";
  }
  const wider = new Uint8ClampedArray(2);
  wider[0] = 1000.1;
  wider[1] = NaN;
  if (wider[0] !== 255 || wider[1] !== 0) {
    throw "Uint8ClampedArray range";
  }

  const signed = new Int8Array(3);
  signed[0] = 128;
  signed[1] = -129;
  signed[2] = 300;
  if (signed[0] !== -128 || signed[1] !== 127 || signed[2] !== 44) {
    throw "Int8 wrapping";
  }
  const unsigned = new Uint32Array(2);
  unsigned[0] = -1;
  unsigned[1] = 4294967296 + 5;
  if (unsigned[0] !== 4294967295 || unsigned[1] !== 5) {
    throw "Uint32 wrapping";
  }

  const half = new Float16Array(2);
  half[0] = 1 + Math.pow(2, -11);
  half[1] = 1 + Math.pow(2, -11) + Math.pow(2, -24);
  if (half[0] !== 1 || half[1] !== 1 + Math.pow(2, -10)) {
    throw "Float16 rounding";
  }
  const negative = new Float16Array([-0]);
  if (negative[0] !== 0 || 1 / negative[0] !== -Infinity) {
    throw "Float16 negative zero";
  }
  const wide = new Float32Array(1);
  wide[0] = 1 / 3;
  if (wide[0] !== 0.3333333432674408) {
    throw "Float32 rounding";
  }

  const big = new BigUint64Array(1);
  big[0] = -1n;
  if (big[0] !== 18446744073709551615n) {
    throw "BigUint64 wrapping";
  }
  const bigSigned = new BigInt64Array([-9223372036854775809n]);
  if (bigSigned[0] !== 9223372036854775807n) {
    throw "BigInt64 wrapping";
  }
  let rejected = false;
  try {
    big[0] = 1;
  } catch (error) {
    rejected = error instanceof TypeError;
  }
  if (!rejected) {
    throw "a Number cannot store into a BigInt element";
  }
  return "ok";
}
