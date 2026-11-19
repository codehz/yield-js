// @returns true
export function run() {
  if (!Object.is(Number(), 0) || !Object.is(Number(" -0 "), -0)) return false;
  if (Number(null) !== 0 || Number(true) !== 1 || Number(false) !== 0) return false;
  if (Number(" 0x20 ") !== 32 || Number({valueOf() { return "3.5"; }}) !== 3.5) return false;
  if (!Number.isNaN(Number(undefined)) || !Number.isNaN(Number("2xyz"))) return false;
  if (Number.prototype.constructor !== Number || Object.getPrototypeOf(Number.prototype) !== Object.prototype) return false;
  if ((1) instanceof Number || Object.is(Number.prototype, 0) || !(Object.create(Number.prototype) instanceof Number)) return false;
  if ((1).missing !== undefined || (2).valueOf() !== 2 || !Object.is((-0).valueOf(), -0)) return false;
  let n = 4;
  Number.prototype.extra = "shared";
  if (n.extra !== "shared" || n.toString !== Number.prototype.toString) return false;
  Object.defineProperty(Number.prototype, "doubled", {get() { return this.valueOf() * 2; }});
  if (n.doubled !== 8 || n["constructor"] !== Number) return false;
  let errors = 0;
  try { new Number(4); } catch (e) { if (e instanceof TypeError) errors++; }
  try { Number(Symbol()); } catch (e) { if (e instanceof TypeError) errors++; }
  try { n.extra = "changed"; } catch (e) { if (e instanceof TypeError) errors++; }
  try { n.other = 0; } catch (e) { if (e instanceof TypeError) errors++; }
  try { Number.prototype.valueOf(); } catch (e) { if (e instanceof TypeError) errors++; }
  return errors === 5 && n.extra === "shared" && n.other === undefined;
}
