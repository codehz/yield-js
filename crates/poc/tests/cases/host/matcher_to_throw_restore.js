// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 2]
// @fulfilled true

import { expect, wait } from 'test';

export async function run() {
  const message = 'captured strawberry';
  const matcher = expect(() => { throw new Error(message); });
  const regexp = /strawberry$/;
  const noMessage = new TypeError();
  Object.setPrototypeOf(noMessage, null);
  await wait('a');
  matcher.toThrow(regexp);
  expect(() => { throw noMessage; }).toThrow(/^$/);
  return true;
}
