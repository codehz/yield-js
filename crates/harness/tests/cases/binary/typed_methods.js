// @returns "ok"

function eq(actual, expected, label) {
  if (actual !== expected) {
    throw label + ": expected " + expected + ", got " + actual;
  }
}

function throwsError(kind, fn, label) {
  try {
    fn();
  } catch (error) {
    if (!(error instanceof kind)) {
      throw label + ": wrong error type";
    }
    return;
  }
  throw label + ": expected a throw";
}

function join(view) {
  return Array.from(view).join(",");
}

export function run() {
  eq(typeof Uint8Array, "function", "constructor type");
  throwsError(TypeError, () => Uint8Array(2), "call requires new");
  eq(Uint8Array.BYTES_PER_ELEMENT, 1, "constructor element size");
  eq(Uint16Array.BYTES_PER_ELEMENT, 2, "u16 element size");
  eq(Float64Array.prototype.BYTES_PER_ELEMENT, 8, "prototype element size");
  eq(BigInt64Array.BYTES_PER_ELEMENT, 8, "bigint element size");

  eq(join(new Uint8Array([1.5, -1, 256, "3"])), "1,255,0,3", "numeric conversion");
  eq(join(new Uint8ClampedArray([1.5, 2.5, -1, 300])), "2,2,0,255", "clamped rounding");
  eq(join(new Uint16Array(new Uint8Array([1, 2]))), "1,2", "copy from view");
  eq(join(new Uint8Array({ length: 2, 0: "5", 1: 2 })), "5,2", "array-like");
  eq(join(new Uint8Array(3)), "0,0,0", "fresh buffer is zeroed");
  eq(new Uint8Array(new ArrayBuffer(4), 1, 2).byteOffset, 1, "constructor offset");
  eq(new Uint8Array(new ArrayBuffer(4), 1, 2).length, 2, "constructor length");
  throwsError(RangeError, () => new Uint8Array(new ArrayBuffer(2), 3), "offset overflow");
  throwsError(RangeError, () => new Uint8Array(-1), "negative length");

  eq(join(new Float16Array([1.5, 2])), "1.5,2", "float16 exact values");
  eq(Math.abs(new Float16Array([1.1])[0] - 1.1) < 0.001, true, "float16 rounding");
  eq(new Float16Array([65504])[0], 65504, "float16 max");
  eq(Infinity !== new Float16Array([Infinity])[0], false, "float16 infinity");
  eq(join(new Float64Array([Math.NaN])), "NaN", "nan round trip");

  const bigints = new BigInt64Array([1n, -2n]);
  eq(join(bigints), "1,-2", "bigint values");
  eq(typeof bigints[0], "bigint", "bigint element type");
  throwsError(TypeError, () => { new BigInt64Array(1)[0] = 1; }, "number write");
  throwsError(TypeError, () => { new BigInt64Array([1]); }, "number input");
  throwsError(TypeError, () => { new BigInt64Array(1).fill(1); }, "number fill");

  const shared = new Uint8Array([1, 2, 3, 4]);
  shared.set(shared.subarray(0, 3), 1);
  eq(join(shared), "1,1,2,3", "set handles overlap");
  shared.set([9, 9], 0);
  eq(join(shared), "9,9,2,3", "set from array");
  throwsError(
    RangeError,
    () => shared.set([1, 2, 3], 2),
    "set overflow",
  );

  const sliced = shared.subarray(1, 3);
  sliced[0] = 7;
  eq(shared[1], 7, "subarray shares the buffer");
  eq(sliced.buffer === shared.buffer, true, "same buffer");
  const copy = shared.slice(1, 3);
  copy[0] = 5;
  eq(shared[1], 7, "slice copies");

  const floats = new Float64Array([3, Number.NaN, -0, 0, 1, -2]);
  const sorted = floats.toSorted();
  eq(join(sorted), "-2,0,0,1,3,NaN", "default sort order");
  eq(1 / sorted[1], -Infinity, "sorted negative zero first");
  eq(1 / sorted[2], Infinity, "sorted positive zero second");
  eq(join(floats), "3,NaN,0,0,1,-2", "toSorted does not mutate");
  const ordered = new Float64Array([3, 1, 2]);
  ordered.sort((left, right) => right - left);
  eq(join(ordered), "3,2,1", "comparator sort");
  const reversed = ordered.toReversed();
  eq(join(reversed), "1,2,3", "toReversed");
  eq(join(ordered.reverse()), "1,2,3", "reverse mutates");
  eq(join(ordered), "1,2,3", "toReversed leaves the source alone");

  const replaced = new Uint8Array([1, 2, 3]).with(1, 9);
  eq(join(replaced), "1,9,3", "with");
  throwsError(RangeError, () => new Uint8Array(2).with(2, 0), "with out of range");

  const filled = new Uint8Array(5);
  filled.fill(4, 1, 3);
  eq(join(filled), "0,4,4,0,0", "fill");
  const within = new Uint8Array([1, 2, 3, 4, 5]);
  within.copyWithin(0, 3);
  eq(join(within), "4,5,3,4,5", "copyWithin");

  const values = new Int32Array([1, 2, 2, 3]);
  eq(values.at(-1), 3, "at negative");
  eq(values.at(9), undefined, "at past the end");
  eq(values.indexOf(2), 1, "indexOf");
  eq(values.lastIndexOf(2), 2, "lastIndexOf");
  eq(values.includes(Number.NaN), false, "includes NaN");
  eq(new Float64Array([Number.NaN]).includes(Number.NaN), true, "includes NaN in floats");
  eq(values.find((value) => value > 2), 3, "find");
  eq(values.findLast((value) => value < 3), 2, "findLast");
  eq(values.findIndex((value) => value > 2), 3, "findIndex");
  eq(values.findLastIndex((value) => value < 3), 2, "findLastIndex");
  eq(values.every((value) => value > 0), true, "every");
  eq(values.some((value) => value > 2), true, "some");
  let sum = 0;
  values.forEach((value) => { sum += value; });
  eq(sum, 8, "forEach");
  eq(values.reduce((total, value) => total + value, 0), 8, "reduce");
  eq(values.reduce((total, value) => total + value), 8, "reduce without initial");
  eq(values.reduceRight((total, value) => total - value, 0), -8, "reduceRight");
  eq(join(values.map((value) => value * 2)), "2,4,4,6", "map");
  eq(join(values.filter((value) => value !== 2)), "1,3", "filter");
  eq(values.join("-"), "1-2-2-3", "join");
  eq(new Int32Array([1, 2]).toString(), "1,2", "toString");
  eq(join(Array.from(values, (value) => value + 1)), "2,3,3,4", "Array.from");
  eq(join(new Int32Array([4, 5]).map((value) => value + 1)), "5,6", "map keeps the type");

  eq(join(Array.from(new Uint8Array([5, 6]).values())), "5,6", "values iterator");
  eq(
    Array.from(new Uint8Array([5, 6]).entries()).map((pair) => pair.join(":")).join(";"),
    "0:5;1:6",
    "entries iterator",
  );
  eq(join(new Uint8Array([5, 6]).keys()), "0,1", "keys iterator");
  eq(join([...new Uint8Array([7, 8])]), "7,8", "spread");
  let total = 0;
  for (const byte of new Uint8Array([1, 2, 3])) {
    total += byte;
  }
  eq(total, 6, "for of");

  eq(join(Uint8Array.of(1, 2, 3)), "1,2,3", "of");
  eq(join(Uint8Array.from([1, 2], (value) => value * 2)), "2,4", "from with mapper");
  eq(join(Uint8Array.from("12")), "1,2", "from string");
  eq(Uint8Array.from.call(Int8Array, [3]) instanceof Int8Array, true, "from honors this");

  Object.defineProperty(Uint8Array, Symbol.species, { value: Int8Array, configurable: true });
  const speciesSource = new Uint8Array([1, 2, 3]);
  const speciesSlice = speciesSource.slice(0, 2);
  eq(speciesSlice instanceof Int8Array, true, "slice uses species");
  const speciesMap = speciesSource.map((value) => value);
  eq(speciesMap instanceof Int8Array, true, "map uses species");
  const speciesCopy = speciesSource.with(0, 0);
  eq(speciesCopy instanceof Uint8Array, true, "with ignores species");
  delete Uint8Array[Symbol.species];
  eq(new Uint8Array([1]).slice(0) instanceof Uint8Array, true, "species defaults to this");

  const defined = new Uint8Array(1);
  Object.defineProperty(defined, "0", { value: 5 });
  eq(defined[0], 5, "define an element value");
  for (const descriptor of [
    { configurable: false },
    { enumerable: false },
    { writable: false },
    { get() { return 1; } },
  ]) {
    let rejected = false;
    try {
      Object.defineProperty(defined, "0", descriptor);
    } catch (error) {
      rejected = error instanceof TypeError;
    }
    if (!rejected) {
      throw "an incompatible element descriptor must be rejected";
    }
  }
  for (const descriptor of [{ configurable: true }, { enumerable: true }, { writable: true }, {}]) {
    Object.defineProperty(defined, "0", descriptor);
  }
  eq(defined[0], 5, "compatible element descriptors are accepted");
  throwsError(
    TypeError,
    () => new Uint8Array(2).set(new BigInt64Array(1)),
    "bigint source into a number view",
  );
  throwsError(TypeError, () => new Float64Array(1).set(new BigUint64Array(1)), "bigint source again");
  throwsError(
    TypeError,
    () => Object.freeze(new Uint8Array(1)),
    "freeze with elements",
  );
  const empty = new Uint8Array(0);
  Object.freeze(empty);
  eq(Object.isFrozen(empty), true, "empty view can be frozen");
  throwsError(
    TypeError,
    () => Array.prototype.push.call(new Uint8Array(1), 1),
    "push cannot grow a view",
  );
  eq(Array.prototype.map.call(new Uint8Array([1, 2]), (value) => value + 1).join(","), "2,3", "array map on a view");
  eq(Object.keys(new Uint8Array([1, 2])).join(","), "0,1", "own keys");
  return "ok";
}
