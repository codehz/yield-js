// @returns true
import { bind, getHostWeak, setHostWeak } from 'test';

function expectTypeError(fn) {
  try {
    fn();
    return false;
  } catch (e) {
    return e instanceof TypeError && e.message === 'invalid weak slot target';
  }
}

function checkAccessors(read, write) {
  if (read() !== undefined) {
    return false;
  }
  const object = {};
  write(object);
  if (read() !== object) {
    return false;
  }
  const symbol = Symbol('host-weak');
  write(symbol);
  if (read() !== symbol) {
    return false;
  }
  if (!expectTypeError(() => write(1))) {
    return false;
  }
  if (read() !== symbol) {
    return false;
  }
  if (!expectTypeError(() => write(null))) {
    return false;
  }
  if (read() !== symbol) {
    return false;
  }
  if (!expectTypeError(() => write(Symbol.for('host-weak')))) {
    return false;
  }
  if (read() !== symbol) {
    return false;
  }
  write(undefined);
  return read() === undefined;
}

export function run() {
  const node = bind('weak');
  return (
    checkAccessors(
      () => node.getWeak(),
      (value) => node.setWeak(value),
    ) &&
    checkAccessors(getHostWeak, setHostWeak)
  );
}
