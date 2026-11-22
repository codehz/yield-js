// @fulfilled true

export async function run() {
  if (Array.fromAsync.name !== "fromAsync" || Array.fromAsync.length !== 1) return false;

  let accessed = false;
  let rejected;
  const invalid = Array.fromAsync({ get [Symbol.asyncIterator]() { accessed = true; } }, null);
  try { await invalid; } catch (error) { rejected = error; }
  if (!(rejected instanceof TypeError) || accessed) return false;

  const p = Promise.resolve(3);
  let index = 0;
  const asyncSource = {
    [Symbol.asyncIterator]() {
      return {
        next() {
          index++;
          if (index === 1) return Promise.resolve({ value: p, done: false });
          return { done: true, get value() { throw "read after done"; } };
        }
      };
    },
    get [Symbol.iterator]() { throw "sync iterator used"; }
  };
  if ((await Array.fromAsync(asyncSource))[0] !== p) return false;
  index = 0;
  const mapped = await Array.fromAsync(asyncSource, (item, k) => item === p && k === 0);
  if (!mapped[0]) return false;

  let trace = "";
  const like = {
    get length() { trace += "l"; return 2; },
    get 0() { trace += "0"; return { then(resolve) { trace += "t"; resolve(7); } }; }
  };
  function Result(length) {
    trace += "c";
    this.initialLength = length;
  }
  const pending = Array.fromAsync.call(Result, like, (value, k) => {
    trace += "m";
    return k === 1 ? 9 : value;
  });
  if (trace !== "lc0") return false;
  const result = await pending;
  return trace === "lc0tmm" && result.initialLength === 2 && result.length === 2
    && result[0] === 7 && result[1] === 9;
}
