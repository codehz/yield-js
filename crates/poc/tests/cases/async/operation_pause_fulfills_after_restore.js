// @pending ["pause:a"]
// @restore
// @fulfilled true

import { pause } from 'test';

export async function run() {
  const value = await pause('a');
  return value === undefined;
}
