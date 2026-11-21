// @pending ["reject:a"]
// @restore
// @fulfilled undefined

import { expect, rejectAfterRestore } from 'test';

export async function run() {
  let outcome;
  try {
    outcome = { fulfilled: await rejectAfterRestore('a') };
  } catch (error) {
    outcome = { rejected: error };
  }
  expect(outcome).toEqual({ rejected: 'reject rejected during restore' });
}
