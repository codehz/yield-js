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
  expect(anonymous.name).toBe('anonymous');
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
  expect(expect.name).toBe('expect');
  expect(expect.length).toBe(1);
  const hostProto = Object.getPrototypeOf(expect(1));
  expect(hostProto.toBe.name).toBe('toBe');
  expect(hostProto.toBe.length).toBe(1);
  const hostGetter = Object.getOwnPropertyDescriptor(hostProto, 'value').get;
  expect(hostGetter.name).toBe('get value');
  expect(hostGetter.length).toBe(0);
  const make = k => ({[k]() {}});
  let conversions = 0;
  const key = {[Symbol.toPrimitive]() { conversions++; return 'literal@123'; }};
  const first = make(key);
  expect(conversions).toBe(1);
  expect(first['literal@123'].name).toBe('literal@123');
  expect(make('other').other.name).toBe('other');
  const missing = Symbol();
  const empty = Symbol('');
  expect(make(missing)[missing].name).toBe('');
  expect(make(empty)[empty].name).toBe('[]');
  const utf16 = {'\uD800'() {}};
  expect(utf16['\uD800'].name).toBe('\uD800');
  const access = {get [empty]() { return 3; }};
  expect(Object.getOwnPropertyDescriptor(access, empty).get.name).toBe('get []');
  class Fields {
    #private = () => 1;
    [empty] = class { static observed = this.name; };
    getPrivate() { return this.#private; }
  }
  const fields = new Fields();
  expect(fields.getPrivate().name).toBe('#private');
  expect(fields[empty].observed).toBe('[]');
  const computed = 'observed name';
  expect(({[computed]: class { static observed = this.name; }})[computed].observed).toBe(computed);
  expect(({[computed]: class { static name = 7; }})[computed].name).toBe(7);
  const {def = (() => 1)} = {};
  let assigned;
  ({assigned = function() {}} = {});
  const withDefault = (param = function() {}) => param;
  expect(def.name).toBe('def');
  expect(assigned.name).toBe('assigned');
  expect(withDefault().name).toBe('param');
  let logical;
  logical ??= () => 1;
  expect(logical.name).toBe('logical');
  const obj = {};
  obj.x = () => 1;
  expect(obj.x.name).toBe('');
  const choose = true ? () => 1 : () => 2;
  expect(choose.name).toBe('');
  let indirect = function() { return indirect; };
  const saved = indirect;
  indirect = 41;
  expect(saved()).toBe(41);
}
