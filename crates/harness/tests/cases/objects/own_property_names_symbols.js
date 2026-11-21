// @fulfilled "ok"

export async function run() {
  const symbol = Symbol("s");
  const target = {};
  Object.defineProperty(target, "hidden", { value: 1, enumerable: false });
  target[0] = "zero";
  target.visible = 2;
  target[symbol] = 3;

  const names = Object.getOwnPropertyNames(target);
  if (names.join(",") !== "0,hidden,visible") return names.join(",");
  const symbols = Object.getOwnPropertySymbols(target);
  if (symbols.length !== 1 || symbols[0] !== symbol) return "symbols";

  const text = Object.getOwnPropertyNames("ab");
  if (text.join(",") !== "0,1,length") return text.join(",");
  if (Object.getOwnPropertySymbols("ab").length !== 0) return "string symbols";

  let trapped = 0;
  const proxy = new Proxy(target, {
    ownKeys() {
      trapped += 1;
      return Reflect.ownKeys(target);
    },
  });
  if (Object.getOwnPropertyNames(proxy).join(",") !== "0,hidden,visible") {
    return "proxy names";
  }
  const proxySymbols = Object.getOwnPropertySymbols(proxy);
  if (proxySymbols.length !== 1 || proxySymbols[0] !== symbol || trapped !== 2) {
    return "proxy symbols";
  }

  for (const key of ["getOwnPropertyNames", "getOwnPropertySymbols"]) {
    try {
      Object[key](null);
      return key;
    } catch (error) {
      if (!(error instanceof TypeError)) return key + " type";
    }
  }
  return "ok";
}
