// @rejected "TypeError: Cannot assign to read only property 'prototype'"

async function run() {
  Promise.prototype = {};
  return 1;
}
