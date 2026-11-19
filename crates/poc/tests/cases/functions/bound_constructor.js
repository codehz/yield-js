// @returns true

function Target(a, b, c) { this.args = [a, b, c].join(','); }
function Alternate() { throw 'newTarget body must not run'; }

export function run() {
  const receiver = {};
  const bound = Target.bind(receiver, 1, 2);
  if (bound.name !== 'bound Target' || bound.length !== 1 ||
      Target.bind(null, 1, 2, 3, 4).length !== 0 || 'prototype' in bound) return false;

  const instance = new bound(3);
  const reflected = Reflect.construct(bound, [4]);
  const explicit = Reflect.construct(bound, [5], bound);
  const alternate = Reflect.construct(bound, [6], Alternate);
  return instance.args === '1,2,3' && instance instanceof Target && instance instanceof bound &&
    Object.getPrototypeOf(instance) === Target.prototype &&
    reflected.args === '1,2,4' && Object.getPrototypeOf(reflected) === Target.prototype &&
    explicit.args === '1,2,5' && Object.getPrototypeOf(explicit) === Target.prototype &&
    alternate.args === '1,2,6' && Object.getPrototypeOf(alternate) === Alternate.prototype &&
    receiver.args === undefined;
}
