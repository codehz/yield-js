// @fulfilled "TypeError,method,next,step,done,value,TypeError,TypeError:0"

function broken(stage, closes) {
  return {
    get [Symbol.iterator]() {
      if (stage === "method") throw stage;
      if (stage === "nonobject") return () => 1;
      return () => ({
        get next() {
          if (stage === "next") throw stage;
          return () => {
            if (stage === "step") throw stage;
            if (stage === "bad step") return 1;
            return {
              get done() { if (stage === "done") throw stage; return false; },
              get value() { throw stage; },
            };
          };
        },
        return() { closes.count++; throw "wrong close"; },
      });
    },
  };
}

export async function run() {
  const closes = { count: 0 };
  const inputs = [1];
  for (const stage of ["method", "next", "step", "done", "value", "nonobject", "bad step"]) {
    inputs[inputs.length] = broken(stage, closes);
  }
  const errors = [];
  for (const input of inputs) {
    // Acquisition and step failures must reject, not throw at the call site.
    const result = Promise.all(input);
    try { await result; }
    catch (error) { errors[errors.length] = typeof error === "string" ? error : error.name; }
  }
  return errors.join(",") + ":" + closes.count;
}
