// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 2]
// @fulfilled true

import { expect, allocError, wait, ParseError, HostDOMException } from 'test';

export async function run() {
  const saved = new ParseError('snapshot failure', { cause: 'stable' });
  const before = allocError('before restore');
  const dom = new HostDOMException('lost', 'AbortError');
  await wait('a');
  expect(Error.isError(saved)).toBe(true);
  expect(saved.message).toBe('snapshot failure');
  expect(saved.cause).toBe('stable');
  expect(saved instanceof ParseError).toBe(true);
  expect(before instanceof ParseError).toBe(true);
  expect(before.message).toBe('before restore');
  // The restored class shares one prototype pair with fresh host allocations.
  expect(Object.getPrototypeOf(saved) === Object.getPrototypeOf(allocError('after'))).toBe(true);
  expect(Object.getPrototypeOf(saved).constructor === ParseError).toBe(true);
  expect(Error.isError(dom) && dom instanceof HostDOMException).toBe(true);
  expect(dom.name + ':' + dom.message + ':' + dom.code).toBe('AbortError:lost:20');
  return true;
}
