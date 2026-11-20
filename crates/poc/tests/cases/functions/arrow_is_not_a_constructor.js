// @fulfilled undefined

import { expect } from 'test';

export async function run() {
  try {
    await constructArrow();
  } catch (e) {
    expect(typeof e).toBe("object");
    expect(e.name).toBe("TypeError");
    expect(e.message).toBe("value is not a constructor");
    return;
  }
  expect("construction succeeded").toBe("construction threw");
}

async function constructArrow() {
  const arrow = () => 1;
  return new arrow();
}
