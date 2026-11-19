// @returns "21|a|true|true|true|true|汉|true|true|boom|true"

export function run() {
  const sym = Symbol("key");
  const proto = { inherited: 1, b: 99 };
  const source = Object.create(proto);
  let order = "";
  Object.defineProperties(source, {
    b: { value: 2, enumerable: true, configurable: true },
    c: { value: 3, enumerable: true, configurable: true },
    hidden: { value: 4 },
    2: { enumerable: true, get() {
      order += "2";
      delete this.b;
      Object.defineProperty(this, "c", { enumerable: false });
      this.added = 6;
      return 20;
    } },
  });
  Object.defineProperty(source, "a", { enumerable: true, get() {
    order += "1";
    return "a";
  } });
  source[sym] = 7;
  Object.defineProperty(source, "__proto__", { value: 8, enumerable: true });
  const sparse = [0, 1];
  delete sparse[0];
  const result = { set a(value) { throw "setter called"; }, ...source, ...sparse, ...null, ...undefined, ...true };
  const desc = Object.getOwnPropertyDescriptor(result, "a");
  const text = { ..."汉字" };
  let error;
  try {
    const fail = { ...{ get x() { throw "boom"; } }, later: (order = "wrong") };
  } catch (e) {
    error = e;
  }
  let surrogate;
  try {
    const fail = { ..."😀" };
  } catch (e) {
    surrogate = e.name === "TypeError";
  }
  return order + "|" + result.a + "|" + (!("b" in result) && !("c" in result)) + "|" +
    (!("added" in result) && !("inherited" in result) && !("hidden" in result)) + "|" +
    (!(0 in result) && result[1] === 1 && result[2] === 20 && !("length" in result)) + "|" +
    (result[sym] === 7 && desc.writable && desc.enumerable && desc.configurable) + "|" + text[0] + "|" +
    (Reflect.ownKeys(result)[4] === sym) + "|" +
    (result.__proto__ === 8 && Object.getPrototypeOf(result) === Object.prototype) + "|" + error + "|" + surrogate;
}
