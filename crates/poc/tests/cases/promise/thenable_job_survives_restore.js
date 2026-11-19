// @pending ["wait:thenable"]
// @restore
// @fulfill ["wait:thenable", 4]
// @fulfilled "get;return;then;tick;inner;9"

export async function run() {
  let trace = "";
  const obj = {
    get then() {
      trace += "get;";
      return function(resolve, reject) {
        trace += this === obj ? "then;" : "wrong receiver;";
        wait("thenable").then(value => {
          resolve({then(accept) { trace += "inner;"; accept(value + 5); }});
          reject("ignored");
        });
      };
    }
  };
  async function consume() { return await obj; }
  const result = consume();
  trace += "return;";
  queueMicrotask(() => { trace += "tick;"; });
  const value = await result;
  return trace + value;
}
