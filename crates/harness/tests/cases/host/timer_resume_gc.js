// @advance 30
// @restore
// @advance 70
// @fulfilled 42
import { spawnDelayedTask, cancelDelayedTask, sleep, gc } from 'test';
let total = 0;
function schedule() {
  const box = { n: 40 };
  spawnDelayedTask(() => { gc(); total += box.n; }, 100);
  return spawnDelayedTask(() => { total += 1000; }, 100);
}
export async function run() {
  const h = schedule();
  const p = sleep(100);
  if (!(p instanceof Promise)) throw 'not a VM Promise';
  cancelDelayedTask(h); cancelDelayedTask(h); gc();
  await p;
  return total + 2;
}
