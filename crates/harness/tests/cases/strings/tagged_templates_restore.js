// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 2]
// @fulfilled true

import { expect, wait } from 'test';

function tag(strings) {
  return strings;
}

function site() {
  return tag`a\n${1}`;
}

function invalidSite() {
  return tag`\unicode`;
}

export async function run() {
  let before = site();
  let invalid = invalidSite();
  await wait("a");
  expect(site()).toBe(before);
  expect(invalidSite()).toBe(invalid);
  expect(Object.isFrozen(before)).toBe(true);
  expect(before[0]).toBe("a\n");
  expect(before.raw[0]).toBe("a\\n");
  expect(invalid[0]).toBe(undefined);
  expect(invalid.raw[0]).toBe("\\unicode");
  return true;
}
