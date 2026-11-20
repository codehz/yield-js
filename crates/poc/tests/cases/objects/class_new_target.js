// @pending ["wait:target"]
// @restore
// @fulfill ["wait:target",0]
// @fulfilled undefined

import { expect, wait } from 'test';

function F() { return () => new.target; }
function G() { throw 'newTarget body must not run'; }
function Default(value = new.target) { this.target = value; }
function Nested() {
  function Inner() { return () => new.target; }
  return { called: Inner(), constructed: new Inner() };
}
function AsyncArrow() {
  return async () => {
    await wait('target');
    return new.target;
  };
}

export async function run() {
  expect(F()()).toBe(undefined);
  expect((new F())()).toBe(F);
  const reflected = Reflect.construct(F, [], G);
  expect(reflected()).toBe(G);

  const bound = F.bind(null);
  expect((new bound())()).toBe(F);
  expect(Reflect.construct(bound, [], G)()).toBe(G);
  expect(Reflect.construct(F, [], bound)()).toBe(bound);

  expect((new Default()).target).toBe(Default);
  const receiver = { constructor: G };
  Default.call(receiver);
  expect(receiver.target).toBe(undefined);
  const nested = new Nested();
  expect(nested.called()).toBe(undefined);
  expect(nested.constructed().name).toBe('Inner');
  const object = { method() { return () => new.target; } };
  expect(object.method()()).toBe(undefined);

  const asyncArrow = Reflect.construct(AsyncArrow, [], G);
  expect(await asyncArrow()).toBe(G);
  expect(reflected()).toBe(G);
}
