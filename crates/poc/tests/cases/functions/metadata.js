// @returns undefined

import { expect } from 'test';

function declared(first, {value}, third = 0, after, ...rest) {}

export function run() {
  const expression = function explicit(a, ...rest) {};
  const anonymous = function(a) {};
  const arrow = (a, b = 0, ...rest) => 0;
  const restOnly = function(...args) {};
  const asyncFunction = async function namedAsync(a, b) {};
  const methods = {method(a, ...rest) {}, ['computed'](a, b) {}};
  expect(declared.name).toBe('declared');
  expect(declared.length).toBe(2);
  expect(expression.name).toBe('explicit');
  expect(expression.length).toBe(1);
  expect(anonymous.name).toBe('function');
  expect(anonymous.length).toBe(1);
  expect(arrow.name).toBe('arrow');
  expect(arrow.length).toBe(1);
  expect(restOnly.length === 0).toBe(true);
  expect(asyncFunction.name).toBe('namedAsync');
  expect(asyncFunction.length).toBe(2);
  expect(methods.method.name).toBe('method');
  expect(methods.method.length).toBe(1);
  expect(methods.computed.name).toBe('computed');
  expect(methods.computed.length).toBe(2);
}
