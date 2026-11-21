// @returns undefined

import { expect } from 'test';

export { probe as run };
const Promise = 7;
const top = this;
function probe() {
  expect(Promise).toBe(7);
  expect(top).toBe(undefined);
  expect(typeof run).toBe('undefined');
}
