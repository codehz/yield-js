// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const matcher = expect(1);
  expect(matcher.value).toBe(1);
  matcher.value = { n: 2 };
  await wait('a');
  matcher.toEqual({ n: 2 });
  expect(matcher.value.n).toBe(2);
}
