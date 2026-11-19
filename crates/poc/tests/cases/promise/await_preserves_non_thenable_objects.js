// @pending ["wait:objects"]
// @restore
// @fulfill ["wait:objects", 0]
// @fulfilled true

export async function run() {
  const obj = {value: 7};
  const array = [3];
  const fn = () => 5;
  const native = bind("await-node");
  let reads = 0;
  const noThen = {get then() { reads += 1; return false; }};
  let tick = false;
  queueMicrotask(() => { tick = true; });
  const result = await obj;
  if (!tick || result !== obj || (await array) !== array || (await fn) !== fn ||
      (await native) !== native || (await noThen) !== noThen || reads !== 1) return false;
  await wait("objects");
  return result === obj && result.value === 7;
}
