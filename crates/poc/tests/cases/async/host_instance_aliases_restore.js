// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled true

import { expect, wait } from 'test';

export async function run() {
  const obj = { n: 1 };
  obj.self = obj;
  const matcher = expect(obj);
  obj.matcher = matcher;
  const compare = matcher.toEqual.bind(matcher);
  await wait('a');
  obj.n = 2;
  compare(obj);
  matcher.toEqual(obj);
  return obj.matcher === matcher && obj.self === obj && matcher.toEqual === obj.matcher.toEqual;
}
