// @returns undefined

import { expect } from 'test';

export function run() {
  const contexts = [];
  const parsed = JSON.parse(' {"num":1e+02,"str":"😀\\ud800","nil":null,"arr":[true,-0],"duplicate":0,"dupli\\u0063ate":2.00} ', function(key, value, context) {
    expect(arguments.length).toBe(3);
    expect(Object.getPrototypeOf(context)).toBe(Object.prototype);
    contexts.push(context);
    if (key === 'num') {
      const descriptor = Object.getOwnPropertyDescriptor(context, 'source');
      expect(descriptor.value).toBe('1e+02');
      expect(descriptor.writable && descriptor.enumerable && descriptor.configurable).toBe(true);
    }
    if (key === 'str') expect(context.source).toBe('"😀\\ud800"');
    if (key === 'nil') expect(context.source).toBe('null');
    if (key === '0') expect(context.source).toBe('true');
    if (key === '1') expect(context.source).toBe('-0');
    if (key === 'duplicate') expect(context.source).toBe('2.00');
    if (key === 'arr' || key === '') expect(Object.keys(context).length).toBe(0);
    return value;
  });
  expect(parsed.num).toBe(100);
  expect(parsed.duplicate).toBe(2);
  expect(contexts[0] === contexts[1]).toBe(false);
  expect(JSON.parse(' \n 1e999 \t', (key, value, context) => {
    expect(value).toBe(Infinity);
    return context.source;
  })).toBe('1e999');
  expect(JSON.parse('1', () => undefined)).toBe(undefined);

  const changed = JSON.parse('{"first":0,"zero":-0,"same":2.00,"nested":{"leaf":3},"getter":4}', function(key, value, context) {
    if (key === 'first') {
      this.zero = 0;
      this.same = 2;
      this.nested = { leaf: 3 };
      Object.defineProperty(this, 'getter', { get() { return 4; } });
    }
    if (key === 'zero' || key === 'leaf') expect(Object.hasOwn(context, 'source')).toBe(false);
    if (key === 'same') expect(context.source).toBe('2.00');
    if (key === 'getter') {
      expect(context.source).toBe('4');
      return 5;
    }
    return value;
  });
  expect(changed.getter).toBe(5);

  let resultHolder;
  const result = JSON.parse('{"first":0,"child":{"leaf":1}}', function(key, value) {
    if (key === 'first') resultHolder = this;
    if (key === 'leaf') resultHolder.child = 9;
    if (key === 'child') expect(typeof value).toBe('object');
    return value;
  });
  expect(result.child.leaf).toBe(1);
}
