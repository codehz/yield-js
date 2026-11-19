// @pending ["wait:keys"]
// @restore
// @fulfill ["wait:keys", 0]
// @fulfilled "Ka;|first|Dgone;Dhidden;PDbefore;|before|Dgone;|gone|Dadded;|added|"

export async function run() {
  let trace = "";
  const parent = { before: 1, hidden: 2, gone: 3 };
  const proto = new Proxy(parent, {
    ownKeys(t) { trace += "P"; return Reflect.ownKeys(t); },
    getOwnPropertyDescriptor(t, k) {
      if (k !== "hidden") trace += "D" + k + ";";
      return Reflect.getOwnPropertyDescriptor(t, k);
    },
  });
  const target = { first: 1, gone: 2, hidden: 3 };
  Object.setPrototypeOf(target, proto);
  const proxy = new Proxy(target, {
    ownKeys() { trace += "K"; return ["first", "gone", "hidden"]; },
    getOwnPropertyDescriptor(t, k) {
      trace += (k === "first" ? "a" : "D" + k) + ";";
      return Reflect.getOwnPropertyDescriptor(t, k);
    },
    get() { throw "for-in must not read values"; },
  });
  for (const key in proxy) {
    trace += "|" + key + "|";
    if (key === "first") {
      await wait("keys");
      delete target.gone;
      Object.defineProperty(target, "hidden", { enumerable: false });
      Object.defineProperty(target, "late", { value: 4, enumerable: true });
      parent.added = 5;
    }
  }
  return trace;
}
