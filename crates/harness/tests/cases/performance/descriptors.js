// @returns true

import { assert } from 'test';

export function run() {
  const global = Object.getOwnPropertyDescriptor(globalThis, 'performance');
  assert(global.enumerable && global.configurable, 'performance global descriptor');
  assert(typeof global.get === 'function' && typeof global.set === 'function', 'replaceable global accessor');
  const original = globalThis.performance;
  assert(original === performance, 'initial global and bare name agree');

  const ctor = Object.getOwnPropertyDescriptor(globalThis, 'Performance');
  assert(ctor.value === Performance && ctor.writable && ctor.configurable, 'Performance global constructor');
  const proto = Performance.prototype;
  const prototype = Object.getOwnPropertyDescriptor(Performance, 'prototype');
  assert(prototype.value === proto && !prototype.writable && !prototype.enumerable && !prototype.configurable,
    'constructor prototype property');
  assert(Object.getPrototypeOf(original) === proto, 'singleton prototype');
  assert(Object.getPrototypeOf(proto) === EventTarget.prototype, 'interface inheritance');
  assert(Object.getPrototypeOf(Performance) === EventTarget, 'constructor inheritance');
  assert(original instanceof Performance && original instanceof EventTarget, 'singleton instanceof');
  const constructor = Object.getOwnPropertyDescriptor(proto, 'constructor');
  assert(constructor.value === Performance && constructor.writable && !constructor.enumerable && constructor.configurable,
    'constructor backlink');

  for (const key of ['now', 'toJSON']) {
    const d = Object.getOwnPropertyDescriptor(proto, key);
    assert(d.writable && d.enumerable && d.configurable, key + ' descriptor');
    assert(d.value.name === key && d.value.length === 0, key + ' function metadata');
  }
  const origin = Object.getOwnPropertyDescriptor(proto, 'timeOrigin');
  assert(typeof origin.get === 'function' && origin.set === undefined && origin.enumerable && origin.configurable,
    'readonly timeOrigin accessor');
  const tag = Object.getOwnPropertyDescriptor(proto, Symbol.toStringTag);
  assert(tag.value === 'Performance' && !tag.writable && !tag.enumerable && tag.configurable, 'interface tag');
  assert(Object.prototype.toString.call(original) === '[object Performance]', 'singleton tag');

  const replacement = {};
  globalThis.performance = replacement;
  const replaced = Object.getOwnPropertyDescriptor(globalThis, 'performance');
  assert(replaced.value === replacement && replaced.writable && replaced.enumerable && replaced.configurable,
    'assignment replaces the accessor with a data property');
  assert(globalThis.performance === replacement, 'replacement is visible through the global object');
  assert(performance === original && original.now() === 0, 'bare name still refers to the original singleton');
  return true;
}
