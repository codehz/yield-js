// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a",0]
// @fulfilled true

import { bind, wait } from 'test';

export async function run() {
  const node = bind('persisted');
  await wait('a');
  return node.label() === 'persisted';
}
