// @pending ["wait:c"]
// @reject ["wait:c", "boom"]
// @fulfilled "boom|c!boom,t:x,l:undefined:true|undefined:true|1"

import { wait } from 'test';

export async function run() {
  async function* g() {
    return 1;
  }
  const it = g();
  const done = await it.next();
  const order = [];
  // Only the first request is rejected by a rejected return argument; the two
  // queued behind it still run against the completed generator.
  const closing = it.return(wait("c"));
  const throwing = it.throw("x");
  const later = it.next();
  closing.then(
    (result) => order.push(`c:${result.value}:${result.done}`),
    (error) => order.push(`c!${error}`)
  );
  throwing.then(
    () => order.push("t:fulfilled"),
    (error) => order.push(`t:${error}`)
  );
  later.then((result) => order.push(`l:${result.value}:${result.done}`));
  let reason;
  try {
    await closing;
  } catch (error) {
    reason = error;
  }
  const rest = await later;
  return `${reason}|${order.join(",")}|${rest.value}:${rest.done}|${done.value}`;
}
