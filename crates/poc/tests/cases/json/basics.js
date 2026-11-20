// @returns true
export function run() {
  if (typeof JSON !== "object" || JSON.prototype !== undefined) return false;
  try { JSON(); return false; } catch (e) { if (!(e instanceof TypeError)) return false; }
  try { new JSON(); return false; } catch (e) { if (!(e instanceof TypeError)) return false; }
  if (JSON.stringify(null) !== "null" || JSON.stringify(true) !== "true") return false;
  if (JSON.stringify(1.5) !== "1.5" || JSON.stringify(-0) !== "0") return false;
  if (JSON.stringify("a\"b\\") !== "\"a\\\"b\\\\\"") return false;
  if (JSON.stringify("\u0001") !== "\"\\u0001\"") return false;
  if (JSON.stringify(undefined) !== undefined || JSON.stringify(() => 1) !== undefined) return false;
  if (JSON.stringify(Symbol()) !== undefined || JSON.stringify(NaN) !== "null") return false;
  if (JSON.stringify(Infinity) !== "null") return false;
  const parsed = JSON.parse('{"b":1,"a":[true,null,false],"__proto__":{"x":1}}');
  if (parsed.b !== 1 || parsed.a[2] !== false) return false;
  if (Object.getPrototypeOf(parsed) !== Object.prototype) return false;
  if (parsed.__proto__.x !== 1 || Object.getPrototypeOf(parsed) === parsed.__proto__) return false;
  try { JSON.parse("{"); return false; } catch (e) { if (!(e instanceof SyntaxError)) return false; }
  try { JSON.parse("1 2"); return false; } catch (e) { if (!(e instanceof SyntaxError)) return false; }
  return true;
}
