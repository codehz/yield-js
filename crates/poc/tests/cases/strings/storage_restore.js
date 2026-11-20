// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 2]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const s = 'éÿ';
  const t = s + 'Ā';
  const obj = { [t.slice(0, 2)]: true };
  await wait('a');
  expect(s.length).toBe(2);
  expect(s.charCodeAt(1)).toBe(255);
  expect(t.charCodeAt(2)).toBe(256);
  expect(obj[s]).toBe(true);
  expect(('Ā' + s).indexOf(s)).toBe(1);
}
