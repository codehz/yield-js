// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  for (const C of [DisposableStack, AsyncDisposableStack]) {
    const asynchronous = C === AsyncDisposableStack;
    const symbol = asynchronous ? Symbol.asyncDispose : Symbol.dispose;
    const release = asynchronous ? 'disposeAsync' : 'dispose';
    for (const action of ['dispose', 'move', 'move-and-dispose']) {
      const stack = new C();
      let trace = '';
      let moved;
      let pending;
      stack.defer(() => { trace += 'old'; });
      const resource = {};
      Object.defineProperty(resource, symbol, {
        get() {
          trace += 'get/';
          if (action === 'dispose') {
            pending = stack[release]();
          } else {
            const first = stack.move();
            moved = first.move();
            if (action === 'move-and-dispose') pending = moved[release]();
          }
          return () => { trace += 'late/'; };
        }
      });
      expect(stack.use(resource)).toBe(resource);
      expect(stack.disposed).toBe(true);
      if (moved) await moved[release]();
      await pending;
      await stack[release]();
      expect(trace).toBe(action === 'move' ? 'get/late/old' : 'get/old');
    }
    const stack = new C();
    const marker = {};
    const resource = {};
    Object.defineProperty(resource, symbol, {
      get() {
        stack[release]();
        throw marker;
      }
    });
    let error;
    try { stack.use(resource); } catch (caught) { error = caught; }
    expect(error).toBe(marker);
    expect(stack.disposed).toBe(true);
  }
}
