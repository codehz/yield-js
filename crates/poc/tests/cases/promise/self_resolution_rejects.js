// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  let finish;
  const direct = new Promise((resolve, reject) => {
    finish = () => { resolve(direct); reject("ignored"); };
  });
  let reads = 0;
  Object.defineProperty(direct, "then", {get() { reads += 1; throw "unexpected getter"; }});
  finish();
  let directError;
  try { await direct; }
  catch (error) { directError = error; }
  expect(directError?.name).toBe("TypeError");
  expect(reads).toBe(0);

  const chain = Promise.resolve(0).then(() => chain);
  let chainError;
  try { await chain; }
  catch (error) { chainError = error; }
  expect(chainError?.name).toBe("TypeError");

  async function returnSelf() { await 0; return pending; }
  const pending = returnSelf();
  let pendingError;
  try { await pending; }
  catch (error) { pendingError = error; }
  expect(pendingError?.name).toBe("TypeError");

  async function returnThenable() { return {then(resolve) { resolve(42); }}; }
  expect(await returnThenable()).toBe(42);
}
