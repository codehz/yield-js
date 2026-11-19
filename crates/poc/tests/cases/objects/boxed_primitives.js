// @returns true

export function run() {
  const empty = Object(null);
  if (Object.getPrototypeOf(empty) !== Object.prototype ||
      Object.getPrototypeOf(Object(undefined)) !== Object.prototype ||
      Object.getPrototypeOf(new Object()) !== Object.prototype ||
      Object(null) === empty) return false;

  const symbol = Symbol("token");
  const primitives = ["abc", 1, false, symbol];
  const prototypes = [String.prototype, Number.prototype, Boolean.prototype, Symbol.prototype];
  const names = ["String", "Number", "Boolean", "Symbol"];
  const texts = ["abc", "1", "false", "Symbol(token)"];
  for (let i = 0; i < primitives.length; i++) {
    const value = primitives[i];
    const box = Object(value);
    const constructed = new Object(value);
    if (typeof box !== "object" || box === value || box === Object(value) ||
        box === constructed || Object(box) !== box || constructed.valueOf() !== value ||
        box.valueOf() !== value || box.toString() !== texts[i] ||
        Object.prototype.toString.call(box) !== "[object " + names[i] + "]" ||
        Object.getPrototypeOf(box) !== prototypes[i] ||
        Object.getPrototypeOf(value) !== prototypes[i] ||
        Object.setPrototypeOf(value, null) !== value ||
        box.hasOwnProperty("valueOf") || box.hasOwnProperty("toString")) {
      return false;
    }
  }

  const string = new String("A😀");
  if (string.valueOf() !== "A😀" || string.toString() !== "A😀" || string.length !== 3 ||
      string[0] !== "A" || !("2" in string) || !Reflect.has(string, "length") ||
      Reflect.has(string, "3")) return false;

  try {
    string[0] = "B";
    return false;
  } catch (error) {
    if (!(error instanceof TypeError) || string[0] !== "A") return false;
  }
  try {
    string.length = 0;
    return false;
  } catch (error) {
    if (!(error instanceof TypeError) || string.length !== 3) return false;
  }
  string.extra = 42;
  if (string.extra !== 42 || !("extra" in string)) return false;

  const number = new Number(1);
  const boolean = new Boolean(false);
  if (number === 1 || number.valueOf() !== 1 || number.toString() !== "1" ||
      number.toFixed(1) !== "1.0" || typeof boolean !== "object" ||
      boolean.valueOf() !== false || boolean.toString() !== "false" ||
      Boolean() !== false || Boolean(0) !== false || Boolean("false") !== true ||
      Boolean(boolean) !== true || Object(symbol).description !== "token" ||
      String(123) !== "123" || Number("2") !== 2) return false;

  try {
    new Symbol("not a constructor");
    return false;
  } catch (error) {
    return error instanceof TypeError;
  }
}
