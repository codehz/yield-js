// @returns true
export function run() {
  return Math.max() === -Infinity && Math.min() === Infinity && Math.hypot() === 0
    && Math.max(1, 3, 2) === 3 && Math.min(1, -3, -2) === -3
    && Object.is(Math.max(-0, 0, -0), 0) && Object.is(Math.max(-0, -0), -0)
    && Object.is(Math.min(0, -0, 0), -0) && Object.is(Math.min(0, 0), 0)
    && Number.isNaN(Math.max(NaN, Infinity)) && Number.isNaN(Math.min(-Infinity, NaN))
    && Math.hypot(3, -4, 12) === 13 && Math.hypot(NaN, -Infinity) === Infinity
    && Number.isNaN(Math.hypot(Number.MAX_VALUE, Number.MAX_VALUE, NaN))
    && Math.hypot(1e300, 1e300) > 1e300 && Math.hypot(1e-300, 1e-300) > 1e-300
    && Math.round(1.5) === 2 && Math.round(-1.5) === -1 && Math.round(0.49999999999999994) === 0
    && Object.is(Math.round(-0.5), -0) && Object.is(Math.round(-0.1), -0)
    && Object.is(Math.round(-0), -0) && Math.round(Infinity) === Infinity && Number.isNaN(Math.round())
    && Object.is(Math.ceil(-0.5), -0) && Object.is(Math.sign(-0), -0)
    && Object.is(Math.abs(-0), 0) && Object.is(Math.fround(-1e-50), -0)
    && Object.is(Math.sqrt(-0), -0) && Number.isNaN(Math.sqrt(-1)) && Math.log(0) === -Infinity
    && Object.is(Math.atan2(-0, 0), -0) && Math.atan2(-0, -0) === -Math.PI
    && Math.atan2(Infinity, Infinity) === Math.PI / 4
    && Math.pow(NaN, 0) === 1 && Number.isNaN(Math.pow(1, NaN)) && Number.isNaN(Math.pow(-1, Infinity))
    && Object.is(Math.pow(-0, 3), -0) && Math.pow(-0, -3) === -Infinity;
}
