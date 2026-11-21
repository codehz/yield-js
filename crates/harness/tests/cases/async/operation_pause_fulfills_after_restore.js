// @pending ["pause:a"]
// @restore
// @fulfilled undefined

import { expect, pause } from 'test';

export async function run() {
  const value = await pause('a');
  expect(value).toBe(undefined);
}
