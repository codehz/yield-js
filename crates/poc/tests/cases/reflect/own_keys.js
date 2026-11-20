// @pending ["wait:keys"]
// @restore
// @fulfill ["wait:keys", 0]
// @fulfilled undefined

import { expect, wait } from 'test';

function Box() {}

export async function run() {
  const first = Symbol("same");
  const second = Symbol("same");
  const obj = {
    z: 0,
    [second]: 0,
    get x() { throw "enumeration must not read this"; },
    a: 0,
    set x(value) {},
    [first]: 0,
    "10": 0,
    "4294967295": 0,
    "2": 0,
    "01": 0,
    "4294967294": 0,
    [Symbol("key only")]: 42,
  };
  obj.z = 1;
  obj[second] = 1;
  Box.prototype.inherited = 1;
  const instance = new Box();
  instance.own = 1;
  const array = [1, 2, 3];
  array.z = 1;
  array[second] = 1;
  array["+1"] = 4;
  array["4294967295"] = 5;
  array.length = 2;
  const ownKeys = Reflect.ownKeys;
  await wait("keys");
  obj.end = 0;
  const keys = ownKeys(obj);
  expect(typeof Reflect).toBe("object");
  expect(ownKeys).toBe(Reflect.ownKeys);
  expect(keys.length).toBe(12);
  expect(keys[0]).toBe("2");
  expect(keys[1]).toBe("10");
  expect(keys[2]).toBe("4294967294");
  expect(keys[3]).toBe("z");
  expect(keys[4]).toBe("x");
  expect(keys[5]).toBe("a");
  expect(keys[6]).toBe("4294967295");
  expect(keys[7]).toBe("01");
  expect(keys[8]).toBe("end");
  expect(keys[9]).toBe(second);
  expect(keys[10]).toBe(first);
  expect(typeof keys[11]).toBe("symbol");
  expect(obj[keys[11]]).toBe(42);
  const own = Reflect.ownKeys(instance);
  expect(own.length).toBe(1);
  expect(own[0]).toBe("own");
  const indices = Reflect.ownKeys(array);
  expect(indices.length).toBe(7);
  expect(indices[0]).toBe("0");
  expect(indices[1]).toBe("1");
  expect(indices[2]).toBe("length");
  expect(indices[3]).toBe("z");
  expect(indices[4]).toBe("+1");
  expect(indices[5]).toBe("4294967295");
  expect(indices[6]).toBe(second);
  expect(array[1]).toBe(2);
  let caught;
  try {
    Reflect.ownKeys("abc");
  } catch (error) {
    caught = error;
  }
  expect(caught && caught.name).toBe("TypeError");
}
