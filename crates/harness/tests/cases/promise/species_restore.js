// @pending ["wait:species"]
// @restore
// @fulfill ["wait:species", 5]
// @fulfilled undefined

import { expect, wait } from 'test';

let constructed;
function Species(executor) {
  constructed = new Promise(executor);
  return constructed;
}

export async function run() {
  const key = Symbol.species;
  const box = { [key]: Species };
  const source = (async () => await wait("species"))();
  source.constructor = box;
  const result = source.then(value => value + 1);
  const final = source.finally(() => 0);
  const used = result !== final && final === constructed;
  expect(used).toBe(true);
  expect(await result).toBe(6);
  expect(await final).toBe(5);
  expect(key).toBe(Symbol.species);
  expect(box[Symbol.species]).toBe(Species);
}
