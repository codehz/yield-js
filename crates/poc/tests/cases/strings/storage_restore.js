// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 2]
// @fulfilled true

import { wait } from 'test';

export async function run() {
  const s = 'éÿ';
  const t = s + 'Ā';
  const obj = { [t.slice(0, 2)]: true };
  await wait('a');
  return s.length === 2 && s.charCodeAt(1) === 255 &&
    t.charCodeAt(2) === 256 && obj[s] === true && ('Ā' + s).indexOf(s) === 1;
}
