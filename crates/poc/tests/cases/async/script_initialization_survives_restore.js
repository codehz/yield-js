// @pending ["wait:boot"]
// @restore
// @fulfill ["wait:boot", 5]
// @fulfilled undefined

import { expect, wait } from 'test';

"use strict";
const early = readLater();
const hoisted = nestedVar;
if (false) {
  var nestedVar = 100;
}
const topThis = () => this;
let total = 2;
const boot = startBoot();
queueMicrotask(() => { total *= 3; });

function readLater() {
  return total;
}

async function startBoot() {
  const increment = await wait("boot");
  total += increment;
}

export async function run() {
  const initialized = total;
  await boot;
  expect(early).toBe(undefined);
  expect(hoisted).toBe(undefined);
  expect(initialized).toBe(6);
  expect(total).toBe(11);
  expect(topThis.call({ n: 1 })).toBe(undefined);
}
