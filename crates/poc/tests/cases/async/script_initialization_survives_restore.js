// @pending ["wait:boot"]
// @restore
// @fulfill ["wait:boot", 5]
// @fulfilled true

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

async function run() {
  const initialized = total;
  await boot;
  return early === undefined && hoisted === undefined && initialized === 6 &&
    total === 11 && topThis.call({ n: 1 }) === undefined;
}
