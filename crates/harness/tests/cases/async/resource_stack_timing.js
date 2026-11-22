// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  const stack = new AsyncDisposableStack();
  let trace = '';
  stack.use({
    [Symbol.asyncDispose]() {
      trace += 'async/';
      return { get then() {
        trace += 'then-get/';
        return resolve => { trace += 'then/'; resolve(); };
      } };
    }
  });
  const fallback = { [Symbol.dispose]() {
    trace += 'sync/';
    return { get then() { throw 'sync disposal result was assimilated'; } };
  } };
  Object.defineProperty(fallback, Symbol.asyncDispose, {
    get() { trace += 'get/'; return null; }
  });
  expect(stack.use(fallback)).toBe(fallback);
  const invalid = {};
  Object.defineProperty(invalid, Symbol.asyncDispose, { value: false });
  Object.defineProperty(invalid, Symbol.dispose, {
    get() { trace += 'unexpected fallback/'; return () => {}; }
  });
  let rejected = false;
  try { stack.use(invalid); } catch (error) { rejected = error instanceof TypeError; }
  expect(rejected).toBe(true);
  const result = stack.disposeAsync();
  expect(trace).toBe('get/sync/');
  await result;
  expect(trace).toBe('get/sync/async/then-get/then/');

  // Empty async records alone collectively cause exactly one await.
  const empty = new AsyncDisposableStack();
  empty.use(null);
  empty.use(undefined);
  trace = '';
  const emptyResult = empty.disposeAsync().then(() => { trace += 'disposed/'; });
  const ticks = Promise.resolve().then(() => { trace += 'tick1/'; }).then(() => { trace += 'tick2/'; });
  await emptyResult;
  await ticks;
  expect(trace).toBe('tick1/disposed/tick2/');

  // A successfully awaited disposer already satisfies earlier empty records.
  const mixed = new AsyncDisposableStack();
  mixed.use(null);
  mixed.defer(() => { trace += 'defer/'; });
  mixed.use(undefined);
  trace = '';
  const mixedResult = mixed.disposeAsync().then(() => { trace += 'disposed/'; });
  expect(trace).toBe('defer/');
  const mixedTicks = Promise.resolve().then(() => { trace += 'tick1/'; }).then(() => { trace += 'tick2/'; });
  await mixedResult;
  await mixedTicks;
  expect(trace).toBe('defer/tick1/disposed/tick2/');
}
