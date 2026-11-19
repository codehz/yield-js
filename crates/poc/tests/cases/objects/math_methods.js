// @returns true
function near(x, y) { return Math.abs(x - y) < 1e-12; }
export function run() {
  return Math.abs('-2') === 2 && Math.acos(1) === 0 && Math.acosh(1) === 0
    && near(Math.asin(1), Math.PI / 2) && near(Math.asinh(Math.sinh(1)), 1)
    && near(Math.atan(1), Math.PI / 4) && near(Math.atanh(Math.tanh(1)), 1)
    && near(Math.atan2(1, -1), 3 * Math.PI / 4) && Math.cbrt(-8) === -2
    && Math.ceil(1.2) === 2 && Math.floor(-1.2) === -2 && Math.trunc(-1.2) === -1
    && Math.cos(0) === 1 && near(Math.cosh(1), (Math.E + 1 / Math.E) / 2)
    && near(Math.exp(1), Math.E) && near(Math.expm1(1), Math.E - 1)
    && near(Math.log(Math.E), 1) && near(Math.log1p(Math.E - 1), 1)
    && Math.log10(100) === 2 && Math.log2(8) === 3
    && Math.pow(2, 8) === 256 && Math.sign(-2) === -1 && Math.sign(Infinity) === 1
    && near(Math.sin(Math.PI / 2), 1) && Math.sqrt(9) === 3 && near(Math.tan(Math.PI / 4), 1)
    && Math.fround(16777217) === 16777216 && Math.fround(Number.MAX_VALUE) === Infinity
    && Math.clz32() === 32 && Math.clz32(4294967297) === 31 && Math.clz32(-1) === 0
    && Math.imul(4294967295, 5) === -5 && Math.imul(2147483648, 2) === 0
    && Math.imul(Infinity, 5) === 0 && Math.imul(3.9, '-2') === -6;
}
