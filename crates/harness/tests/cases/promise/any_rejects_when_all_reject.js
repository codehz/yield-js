// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const notRejected = {};
  let reason = notRejected;
  try { await Promise.any([Promise.reject(1), Promise.reject(2)]); }
  catch (error) { reason = error; }
  expect(reason === notRejected).toBe(false);
}
