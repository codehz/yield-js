// @pending ["wait:registered-members"]
// @restore
// @fulfill ["wait:registered-members", 0]
// @fulfilled true

import { wait } from 'test';

export async function run() {
  const isFinite = Number.isFinite;
  const charAt = String.prototype.charAt;
  await wait('registered-members');
  return isFinite === Number.isFinite
    && charAt === String.prototype.charAt
    && isFinite(3) === true
    && 'abc'.charAt(1) === 'b'
    && (12).toFixed(1) === '12.0'
    && String.fromCharCode(65) === 'A';
}
