// @fulfilled true

export async function run() {
  try {
    await assignPrototype();
  } catch (e) {
    return typeof e === "object" && e.name === "TypeError" &&
      e.message === "Cannot assign to read only property 'prototype'";
  }
  return false;
}

async function assignPrototype() {
  Promise.prototype = {};
}
