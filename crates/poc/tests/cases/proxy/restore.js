// @pending ["wait:proxy"]
// @restore
// @fulfill ["wait:proxy", 0]
// @pending ["wait:again"]
// @restore
// @fulfill ["wait:again", 0]
// @fulfilled undefined

import { expect, wait } from 'test';

function make() {
  let count = 0;
  const target = { value: 7 };
  const handler = {
    ownKeys() { throw "snapshot must not enumerate proxy"; },
    get(t, k, receiver) {
      if (this !== handler || t !== target || receiver !== proxy) throw "identity lost";
      if (k === "count") return count;
      count += 1;
      return t[k] + count;
    },
  };
  const proxy = new Proxy(target, handler);
  return { proxy, alias: proxy, target };
}

export async function run() {
  const saved = make();
  expect(saved.proxy.value).toBe(8);
  await wait("proxy");
  expect(saved.alias).toBe(saved.proxy);
  expect(saved.alias.count).toBe(1);
  saved.target.value = 10;
  expect(saved.alias.value).toBe(12);
  await wait("again");
  expect(saved.alias).toBe(saved.proxy);
  expect(saved.proxy.count).toBe(2);
  expect(saved.proxy.value).toBe(13);
}
