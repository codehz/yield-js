// @fulfilled "ok"

async function* source() {
  yield 6;
  yield 7;
}

function* generated() {
  const captured = [];
  for (let value of [8, 9]) {
    captured.push(() => value);
    yield value;
  }
  return captured.map(fn => fn()).join(",");
}

export async function run() {
  const captured = [];
  for (let value of [1, 2, 3]) captured.push(() => value);
  if (captured.map(fn => fn()).join(",") !== "1,2,3") return "for-of";

  const inLoop = [];
  for (let key in { a: 1, b: 2 }) inLoop.push(() => key);
  if (inLoop.map(fn => fn()).sort().join(",") !== "a,b") return "for-in";

  const counted = [];
  for (let index = 0; index < 3; index++) counted.push(() => index);
  if (counted.map(fn => fn()).join(",") !== "0,1,2") return "for";

  const awaited = [];
  for (let value of [4, 5]) {
    await 0;
    awaited.push(() => value);
  }
  if (awaited.map(fn => fn()).join(",") !== "4,5") return "await";

  const asyncLoop = [];
  for await (let value of source()) asyncLoop.push(() => value);
  if (asyncLoop.map(fn => fn()).join(",") !== "6,7") return "for-await";

  const iterator = generated();
  if (iterator.next().value !== 8) return "generator first";
  iterator.next();
  const done = iterator.next();
  if (!done.done || done.value !== "8,9") return "generator captured";

  const mutated = [];
  for (let value of [1, 2]) {
    value += 10;
    mutated.push(value);
  }
  if (mutated.join(",") !== "11,12") return "mutation";

  return "ok";
}
