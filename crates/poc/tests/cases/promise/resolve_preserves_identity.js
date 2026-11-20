// @fulfilled "constructor;constructor;resumed;7"

export async function run() {
  const source = Promise.resolve(7);
  let trace = "";
  Object.defineProperty(source, "constructor", {
    get() { trace += this === source ? "constructor;" : "wrong;"; return Promise; }
  });
  Object.defineProperty(source, "then", {
    get() { throw "must not read then when reusing a promise"; }
  });
  if (Promise.resolve(source) !== source) throw "lost identity";
  const consume = async () => await source;
  const result = consume();
  if (trace !== "constructor;constructor;") throw trace;
  const value = await result;
  return trace + "resumed;" + value;
}
