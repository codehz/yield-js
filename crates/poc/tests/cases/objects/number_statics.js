// @returns true
export function run() {
  if (Number.EPSILON !== 2.220446049250313e-16 || Number.MAX_VALUE !== 1.7976931348623157e308) return false;
  if (Number.MIN_VALUE !== 5e-324 || Number.MIN_VALUE <= 0) return false;
  if (Number.MAX_SAFE_INTEGER !== 9007199254740991 || Number.MIN_SAFE_INTEGER !== -9007199254740991) return false;
  if (Number.POSITIVE_INFINITY !== Infinity || Number.NEGATIVE_INFINITY !== -Infinity || !Object.is(Number.NaN, NaN)) return false;
  if (!Number.isFinite(1.5) || Number.isFinite(Infinity) || Number.isFinite(-Infinity) || Number.isFinite(NaN)) return false;
  if (!Number.isInteger(-0) || !Number.isInteger(9007199254740992) || Number.isInteger(1.5) || Number.isInteger(Infinity)) return false;
  if (!Number.isSafeInteger(Number.MAX_SAFE_INTEGER) || !Number.isSafeInteger(Number.MIN_SAFE_INTEGER)) return false;
  if (Number.isSafeInteger(9007199254740992) || Number.isSafeInteger(-9007199254740992) || Number.isSafeInteger(1.5)) return false;
  if (!Number.isNaN(NaN) || Number.isNaN(Infinity) || Number.isNaN(0)) return false;
  let nonNumber = {valueOf() { throw "must not coerce"; }};
  return !Number.isFinite("1") && !Number.isFinite(null) && !Number.isFinite(nonNumber)
    && !Number.isInteger("1") && !Number.isInteger(nonNumber)
    && !Number.isSafeInteger("1") && !Number.isSafeInteger(nonNumber)
    && !Number.isNaN("NaN") && !Number.isNaN(undefined) && !Number.isNaN(nonNumber);
}
