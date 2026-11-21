// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 3]
// @fulfilled "a2b3c"

import { wait } from 'test';

export async function run() {
  return `a${2}b${await wait("a")}c`;
}
