// @returns true

export function run() {
  const nan = 0 / 0;
  const negative = -0;
  const positive = 0;
  function reciprocal(flag) {
    let zero;
    if (flag) zero = positive;
    else zero = negative;
    return 1 / zero;
  }
  function localReciprocal(flag) {
    let zero;
    if (flag) zero = 0;
    else zero = -0;
    return 1 / zero;
  }
  return nan !== nan && !(nan < 0) && negative === positive &&
    reciprocal(true) === Infinity && reciprocal(false) === -Infinity &&
    localReciprocal(true) === Infinity && localReciprocal(false) === -Infinity &&
    0x20000000000001n === 9007199254740993n &&
    9007199254740993n !== 9007199254740992n && 1n !== 1 &&
    null !== undefined && undefined === void 0 && "a" === "\x61" && true !== 1;
}
