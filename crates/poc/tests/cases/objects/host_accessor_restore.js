// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled true

import { expect, wait } from 'test';

export async function run() {
  const matcher = expect(1);
  if (matcher.value !== 1) return false;
  matcher.value = { n: 2 };
  await wait('a');
  matcher.toEqual({ n: 2 });
  return matcher.value.n === 2;
}
