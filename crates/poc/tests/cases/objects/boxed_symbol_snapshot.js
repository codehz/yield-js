// @pending ["wait:boxed"]
// @restore
// @fulfill ["wait:boxed", null]
// @fulfilled true

export async function run() {
  const box = Object(Symbol("only reachable through the wrapper"));
  const alias = { box };
  box.self = box;
  await wait("boxed");

  const symbol = box.valueOf();
  const keyed = { [symbol]: 42 };
  return alias.box === box && box.self === box && typeof symbol === "symbol" &&
    box.description === "only reachable through the wrapper" &&
    box.valueOf() === symbol && Object(symbol).valueOf() === symbol &&
    Reflect.ownKeys(keyed)[0] === symbol && keyed[box.valueOf()] === 42 &&
    Object.getPrototypeOf(box) === Symbol.prototype;
}
