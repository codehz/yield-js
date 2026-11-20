// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  try {
    await callWithoutNew();
  } catch (e) {
    expect(typeof e).toBe("object");
    expect(e.name).toBe("TypeError");
    expect(e.message).toBe("Promise constructor requires new");
    return;
  }
  expect("Promise call succeeded").toBe("Promise call threw");
}

async function callWithoutNew() {
  return Promise(function () {});
}
