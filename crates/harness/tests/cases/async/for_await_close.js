// @fulfilled "1|A:true|awaited|B|body|C|D|E|10"
export async function run() {
  const trace = [];

  const a = {
    [Symbol.asyncIterator]() { return this; },
    next() { return Promise.resolve({ value: 1, done: false }); },
    return() {
      trace.push(`A:${this === a}`);
      return { then(resolve) { trace.push("awaited"); resolve({}); } };
    },
  };
  for await (const value of a) { trace.push(value); break; }

  const b = {
    [Symbol.asyncIterator]() { return this; },
    next() { return Promise.resolve({ value: 2, done: false }); },
    return() { trace.push("B"); return Promise.reject("close-b"); },
  };
  try {
    for await (const value of b) { throw "body"; }
  } catch (error) { trace.push(error); }

  const c = {
    [Symbol.asyncIterator]() { return this; },
    next() { return Promise.resolve({ value: 3, done: false }); },
    return() { trace.push("C"); return Promise.reject("close-c"); },
  };
  for await (const value of c) { break; }

  const d = {
    [Symbol.asyncIterator]() { return this; },
    next() { return Promise.resolve({ value: 4, done: false }); },
    return() { trace.push("D"); return 1; },
  };
  for await (const value of d) { break; }

  async function early() {
    const e = {
      [Symbol.asyncIterator]() { return this; },
      next() { return Promise.resolve({ value: 9, done: false }); },
      return() { trace.push("E"); return Promise.resolve({}); },
    };
    for await (const value of e) { return value + 1; }
  }
  const returned = await early();
  return `${trace.join("|")}|${returned}`;
}
