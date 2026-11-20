// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  try {
    await assignPrototype();
  } catch (e) {
    expect(typeof e).toBe("object");
    expect(e.name).toBe("TypeError");
    expect(e.message).toBe("Cannot assign to read only property 'prototype'");
    return;
  }
  expect("prototype assignment succeeded").toBe("prototype assignment threw");
}

async function assignPrototype() {
  Promise.prototype = {};
}
