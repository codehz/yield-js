// @fulfilled undefined

import { expect } from 'test';

function throws(fn, Type) {
  try { fn(); } catch (error) { return error instanceof Type; }
  return false;
}

export async function run() {
  for (const C of [DisposableStack, AsyncDisposableStack]) {
    const async = C === AsyncDisposableStack;
    const release = async ? 'disposeAsync' : 'dispose';
    const symbol = async ? Symbol.asyncDispose : Symbol.dispose;
    const other = async ? new DisposableStack() : new AsyncDisposableStack();
    const p = C.prototype;
    expect(globalThis[C.name]).toBe(C);
    expect(C.length).toBe(0);
    expect(throws(() => C(), TypeError)).toBe(true);
    expect(p.constructor).toBe(C);
    expect(p[symbol]).toBe(p[release]);
    expect(p[release].name).toBe(release);
    for (const name of ['use', 'defer', 'adopt', 'move', release]) {
      const desc = Object.getOwnPropertyDescriptor(p, name);
      expect(desc.enumerable).toBe(false);
      expect(desc.writable).toBe(true);
      expect(desc.configurable).toBe(true);
      expect(desc.value.length).toBe(name === 'adopt' ? 2 : name === 'use' || name === 'defer' ? 1 : 0);
    }
    const disposed = Object.getOwnPropertyDescriptor(p, 'disposed');
    expect(disposed.get.name).toBe('get disposed');
    expect(disposed.set).toBe(undefined);
    expect(disposed.enumerable).toBe(false);
    expect(disposed.configurable).toBe(true);
    expect(throws(() => disposed.get.call(p), TypeError)).toBe(true);
    expect(throws(() => p.use.call(other, null), TypeError)).toBe(true);
    expect(throws(() => p.move.call(new Proxy(new C(), {})), TypeError)).toBe(true);

    class Derived extends C {}
    const s = new Derived();
    expect(s instanceof Derived).toBe(true);
    expect(Object.prototype.toString.call(s)).toBe('[object ' + C.name + ']');
    expect(Object.getOwnPropertyDescriptor(p, Symbol.toStringTag).writable).toBe(false);
    expect(s.disposed).toBe(false);
    expect(s.use(null)).toBe(null);
    expect(s.use(undefined)).toBe(undefined);
    expect(throws(() => s.use(0), TypeError)).toBe(true);
    expect(throws(() => s.adopt(0, null), TypeError)).toBe(true);
    expect(throws(() => s.defer(null), TypeError)).toBe(true);

    let trace = '';
    const resource = {};
    resource[symbol] = function () {
      expect(this).toBe(resource);
      expect(arguments.length).toBe(0);
      trace += 'use';
    };
    expect(s.use(resource)).toBe(resource);
    resource[symbol] = undefined;
    expect(s.adopt('adopt', function (value) {
      expect(this).toBe(undefined);
      expect(arguments.length).toBe(1);
      trace += value + '/';
    })).toBe('adopt');
    let moved;
    expect(s.defer(function () {
      expect(this).toBe(undefined);
      expect(arguments.length).toBe(0);
      expect(moved.disposed).toBe(true);
      expect(throws(() => moved.defer(null), ReferenceError)).toBe(true);
      moved[release]();
      trace += 'defer/';
    })).toBe(undefined);
    Object.defineProperty(s, 'constructor', { get() { throw 'unexpected constructor access'; } });
    moved = s.move();
    expect(Object.getPrototypeOf(moved)).toBe(p);
    expect(s.disposed).toBe(true);
    expect(moved.disposed).toBe(false);
    expect(throws(() => s.move(), ReferenceError)).toBe(true);
    expect(throws(() => s.adopt(1, null), ReferenceError)).toBe(true);
    expect(throws(() => s.use(1), ReferenceError)).toBe(true);
    const first = moved[release]();
    const repeat = moved[release]();
    if (async) {
      expect(first instanceof Promise).toBe(true);
      expect(repeat === first).toBe(false);
      const bad1 = p[release].call(other);
      const bad2 = p[symbol].call(other);
      expect(bad1 instanceof Promise).toBe(true);
      expect(bad1 === bad2).toBe(false);
      const reject1 = bad1.then(undefined, error => error instanceof TypeError);
      const reject2 = bad2.then(undefined, error => error instanceof TypeError);
      expect(await reject1).toBe(true);
      expect(await reject2).toBe(true);
      expect(await repeat).toBe(undefined);
      expect(await first).toBe(undefined);
    } else {
      expect(first).toBe(undefined);
      expect(repeat).toBe(undefined);
      expect(throws(() => p[release].call(other), TypeError)).toBe(true);
    }
    expect(trace).toBe('defer/adopt/use');
  }
}
