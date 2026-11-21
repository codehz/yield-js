// @advance 30
// @restore
// @advance 70
// @fulfilled 42
import { setTimeout, clearTimeout, sleep, gc } from 'test';
let total = 0;
function schedule() {
  const box = { n: 40 };
  setTimeout(() => { gc(); total += box.n; }, 100);
  return setTimeout(() => { total += 1000; }, 100);
}
export async function run() {
  const h = schedule();
  const p = sleep(100);
  if (!(p instanceof Promise)) throw 'not a VM Promise';
  clearTimeout(h); clearTimeout(h); gc();
  await p;
  return total + 2;
}
