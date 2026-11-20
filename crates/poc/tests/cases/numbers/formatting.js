// @returns true
export function run() {
  if ((12.5).toString() !== "12.5" || (12.5).toString(10) !== "12.5" || (12.5).toLocaleString() !== "12.5") return false;
  if ((10).toString(2) !== "1010" || (-31.75).toString("16") !== "-1f" || (35).toString(36) !== "z") return false;
  if ((-0).toString() !== "0" || (-0).toString(2) !== "0") return false;
  if (NaN.toString(16) !== "NaN" || Infinity.toString() !== "Infinity" || (-Infinity).toString(2) !== "-Infinity") return false;
  if ((1.5).toFixed(2) !== "1.50" || (2.75).toFixed() !== "3" || (-0).toFixed(2) !== "0.00") return false;
  if ((0).toFixed(100).length !== 102) return false;
  if ((125).toExponential() !== "1.25e+2" || (0.00125).toExponential(3) !== "1.250e-3" || (-0).toExponential(1) !== "0.0e+0") return false;
  if ((125).toPrecision() !== "125" || (1.25).toPrecision(4) !== "1.250" || (12500).toPrecision(3) !== "1.25e+4") return false;
  if ((0.000000125).toPrecision(3) !== "1.25e-7" || (-0).toPrecision(3) !== "0.00") return false;
  if (Infinity.toFixed(2) !== "Infinity" || (-Infinity).toExponential(2) !== "-Infinity" || NaN.toPrecision(2) !== "NaN") return false;
  if (NaN.toExponential(101) !== "NaN" || Infinity.toPrecision(0) !== "Infinity") return false;
  let errors = 0;
  try { (1).toString(1); } catch (e) { if (e instanceof RangeError) errors++; }
  try { (1).toString(37); } catch (e) { if (e instanceof RangeError) errors++; }
  try { (1).toFixed(-1); } catch (e) { if (e instanceof RangeError) errors++; }
  try { (1).toFixed(101); } catch (e) { if (e instanceof RangeError) errors++; }
  try { (1).toExponential(-1); } catch (e) { if (e instanceof RangeError) errors++; }
  try { (1).toExponential(101); } catch (e) { if (e instanceof RangeError) errors++; }
  try { (1).toPrecision(0); } catch (e) { if (e instanceof RangeError) errors++; }
  try { (1).toPrecision(101); } catch (e) { if (e instanceof RangeError) errors++; }
  try { Reflect.apply(Number.prototype.toFixed, "1", [2]); } catch (e) { if (e instanceof TypeError) errors++; }
  return errors === 9;
}
