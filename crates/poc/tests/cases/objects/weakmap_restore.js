// @pending ["wait:weakmap"]
// @restore
// @fulfill ["wait:weakmap", null]
// @fulfilled true
import { wait } from 'test';

export async function run() {
  const root = {};
  const next = {};
  const tail = new WeakMap([[next, { marker: Symbol("live"), root }]]);
  const head = new WeakMap([[root, { next }]]);
  await wait("weakmap");
  const entry = head.get(root);
  const value = tail.get(entry.next);
  return tail.has(entry.next) && typeof value.marker === "symbol" && value.root === root;
}
