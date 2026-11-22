// @pending ["wait:helpers"]
// @restore
// @fulfill ["wait:helpers",0]
// @fulfilled undefined

import { expect, gc, wait } from 'test';

export async function run() {
  const proto = Object.getPrototypeOf(Object.getPrototypeOf([].values()));
  expect(proto).toBe(Iterator.prototype);
  expect(proto.map.length).toBe(1);
  expect(proto.toArray.length).toBe(0);

  let log = '';
  const pipeline = [1, 2, 3, 4].values()
    .drop(1).take(2)
    .map((v, i) => { log += 'm' + i; return v + i; })
    .filter((v, i) => { log += 'f' + i; return v > 1; })
    .flatMap((v, i) => { gc(); log += 'x' + i; return [v, v * 10]; });
  expect(pipeline[Symbol.iterator]()).toBe(pipeline);
  expect(Object.prototype.toString.call(pipeline)).toBe('[object Iterator Helper]');
  expect(log).toBe('');
  expect(pipeline.next().value).toBe(2);
  expect(log).toBe('m0f0x0');
  await wait('helpers');
  expect(pipeline.toArray().join()).toBe('20,4,40');
  expect(log).toBe('m0f0x0m1f1x1');
  expect(pipeline.next().done).toBe(true);

  expect([5, 6, 7].values().reduce((a, v, i) => a + v * i)).toBe(25);
  expect([].values().reduce(() => 1, undefined)).toBe(undefined);
  let out = '';
  expect([3, 4].values().forEach((v, i) => { out += v + ':' + i; })).toBe(undefined);
  expect(out).toBe('3:04:1');
  expect([].values().some(() => true)).toBe(false);
  expect([].values().every(() => false)).toBe(true);
  expect([2, 5, 8].values().find((v, i) => v === i + 4)).toBe(5);
}
