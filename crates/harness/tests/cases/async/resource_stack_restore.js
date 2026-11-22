// @pending ["wait:b"]
// @restore
// @reject ["wait:b", "B"]
// @fulfilled "BA"

import { expect, wait } from 'test';

export async function run() {
  const stack = new AsyncDisposableStack();
  let trace = '';
  stack.defer(() => {
    trace += 'A';
    throw 'A';
  });
  stack.defer(() => {
    trace += 'B';
    return wait('b');
  });
  try {
    const result = stack.disposeAsync();
    expect(stack.disposed).toBe(true);
    expect(trace).toBe('B');
    await result;
  } catch (error) {
    expect(error instanceof SuppressedError).toBe(true);
    expect(error.error).toBe('A');
    expect(error.suppressed).toBe('B');
    expect(stack.disposed).toBe(true);
    await stack.disposeAsync();
    return trace;
  }
  throw 'expected rejection';
}
