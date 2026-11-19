// @fulfilled "TypeError:TypeError:TypeError:next:done:value:"

function failIfCalled() {
  throw "callee ran";
}

async function run() {
  let trace = "";
  function later() {
    trace += "later";
  }
  function iterable(next) {
    return {
      [Symbol.iterator]() {
        return {
          next: next,
          get return() {
            trace += "closed";
            throw "wrong error";
          },
        };
      },
    };
  }
  const inputs = [
    null,
    { [Symbol.iterator]() { return 3; } },
    iterable(() => 3),
    iterable(() => { throw "next"; }),
    iterable(() => ({ get done() { throw "done"; } })),
    iterable(() => ({ get value() { throw "value"; } })),
  ];
  for (const input of inputs) {
    try {
      failIfCalled(...input, later());
    } catch (error) {
      trace += (typeof error === "string" ? error : error.name) + ":";
    }
  }
  return trace;
}
