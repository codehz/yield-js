// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 0]
// @fulfilled true

import { wait, gc } from 'test';

export async function run() {
  const proto = Function.prototype;
  const fn = String.prototype.slice;
  const stringify = proto.toString;
  const bound = fn.bind('abc', 1);
  const host = wait;
  const parent = Object.create(proto);
  Object.setPrototypeOf(host, parent);
  host.marker = {value: 7};
  delete fn.name;
  Object.defineProperty(fn, 'length', {get() { return 7; }});
  Object.defineProperty(bound, 'name', {value: 'public bound'});
  delete proto.call;
  proto.constructor = 23;
  delete proto.arguments;
  const Retained = class { [Symbol('retained')] = function() {}; };
  const dynamic = ({['dynamic@123']() {}})['dynamic@123'];
  delete dynamic.name;
  gc();
  const make = async () => ({[Symbol('s')]: class extends (await wait('a'), Object) {
    [Symbol('field')] = () => 1;
  }});
  const made = await make();
  gc();
  const key = Reflect.ownKeys(made)[0];
  const cls = made[key];
  const instance = new cls();
  const retained = new Retained();
  const retainedField = Reflect.ownKeys(retained)[0];
  const field = Reflect.ownKeys(instance)[0];
  return fn === String.prototype.slice && !Object.prototype.hasOwnProperty.apply(fn, ['name'])
    && fn.length === 7 && fn.apply('abc', [1]) === 'bc' && fn.call === undefined
    && bound.name === 'public bound' && bound() === 'bc'
    && Reflect.apply(stringify, bound, []) === 'function bound slice() { [native code] }'
    && Reflect.apply(stringify, fn, []) === 'function slice() { [native code] }'
    && Object.getPrototypeOf(host) === parent && host.marker.value === 7
    && Function.prototype === proto && proto.constructor === 23 && !('arguments' in proto)
    && Reflect.apply(stringify, dynamic, []) === 'function dynamic@123() { [native code] }'
    && retained[retainedField].name === '[retained]'
    && cls.name === '[s]' && instance[field].name === '[field]';
}
