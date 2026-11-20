// @pending ["wait:next"]
// @restore
// @fulfill ["wait:next", true]
// @fulfilled "1:1|2:2|done"

export async function run() {
  const iterator = "a1b2".matchAll(/(\d)/g);
  let trace = "";
  const first = iterator.next();
  trace += `${first.value[0]}:${first.value[1]}|`;
  await wait("next");
  const second = iterator.next();
  trace += `${second.value[0]}:${second.value[1]}|`;
  if (iterator.next().done) trace += "done";
  return trace;
}
