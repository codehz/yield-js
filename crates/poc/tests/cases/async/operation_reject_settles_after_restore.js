// @pending ["reject:a"]
// @restore
// @fulfilled true

import { rejectAfterRestore } from 'test';

export async function run() {
  try {
    await rejectAfterRestore('a');
  } catch (error) {
    return error === 'reject rejected during restore';
  }
  return false;
}
