// @pending ["wait:c"]
// @fulfill ["wait:c", 7]
// @fulfilled "0|c:7:true,t:x,l:undefined:true|7:true"

import { wait } from 'test';

export async function run() {
  async function* g() {
    yield 1;
  }
  const it = g();
  await it.next();
  await it.next();
  const order = [];
  // The return argument is still pending, so neither later request may settle
  // before it does.
  const closing = it.return(wait("c"));
  const throwing = it.throw("x");
  const later = it.next();
  closing.then((result) => order.push(`c:${result.value}:${result.done}`));
  throwing.then(
    () => order.push("t:fulfilled"),
    (error) => order.push(`t:${error}`)
  );
  later.then((result) => order.push(`l:${result.value}:${result.done}`));
  for (let step = 0; step < 5; step = step + 1) {
    await Promise.resolve();
  }
  const before = order.length;
  const settled = await closing;
  for (let step = 0; step < 5; step = step + 1) {
    await Promise.resolve();
  }
  return `${before}|${order.join(",")}|${settled.value}:${settled.done}`;
}
