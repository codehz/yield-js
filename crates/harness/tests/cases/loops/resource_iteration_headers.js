// @fulfilled true

export async function run() {
  const expected = "body1/dispose1/body2/dispose2/iteratorReturn/";
  let trace = "";
  function source(asyncIterator, asyncResource) {
    let next = 0;
    const iterator = {
      next() {
        const n = ++next;
        const result = { done: n > 3, value: {
          n,
          [Symbol.dispose]() {
            trace += asyncResource ? "wrong/" : `dispose${n}/`;
          },
          async [Symbol.asyncDispose]() {
            await undefined;
            trace += asyncResource ? `dispose${n}/` : "wrong/";
          },
        } };
        return asyncIterator ? Promise.resolve(result) : result;
      },
      return() {
        trace += "iteratorReturn/";
        return asyncIterator ? Promise.resolve({ done: true }) : { done: true };
      },
    };
    return { [asyncIterator ? Symbol.asyncIterator : Symbol.iterator]() { return iterator; } };
  }
  const captured = [];
  syncSync: for (using resource of source(false, false)) {
    captured.push(() => resource.n);
    trace += `body${resource.n}/`;
    if (resource.n === 1) continue syncSync;
    break syncSync;
  }
  if (trace !== expected || captured[0]() !== 1 || captured[1]() !== 2) return false;
  trace = "";
  for (await using resource of source(false, true)) {
    trace += `body${resource.n}/`;
    if (resource.n === 1) continue;
    break;
  }
  if (trace !== expected) return false;
  trace = "";
  for await (using resource of source(true, false)) {
    trace += `body${resource.n}/`;
    if (resource.n === 1) continue;
    break;
  }
  if (trace !== expected) return false;
  trace = "";
  done: {
    asyncAsync: for await (await using resource of source(true, true)) {
      trace += `body${resource.n}/`;
      if (resource.n === 1) continue asyncAsync;
      break done;
    }
  }
  return trace === expected;
}
