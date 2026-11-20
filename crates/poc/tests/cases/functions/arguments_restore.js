// @pending ["wait:args"]
// @restore
// @fulfill ["wait:args", 2]
// @fulfilled true

import { wait } from 'test';

async function f() {
  const alias = arguments;
  const iter = arguments[Symbol.iterator]();
  const first = iter.next().value;
  const n = await wait("args");
  arguments[1] += n;
  return alias === arguments && first === 4 && iter.next().value === 7
    && alias.length === 2 && alias[0] === 4 && iter.next().done;
}

export function run() { return f(4, 5); }
