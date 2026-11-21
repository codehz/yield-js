// @pending ["reissue:a"]
// @restore
// @fulfill ["reissue:a", 7]
// @fulfilled 7

import { reissue } from 'test';

export async function run() {
  return await reissue('a');
}
