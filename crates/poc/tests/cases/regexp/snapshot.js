// @pending ["wait:regexp"]
// @restore
// @fulfill ["wait:regexp", null]
// @fulfilled undefined

import { expect, wait } from 'test';

export async function run() {
  const re = /foo/g;
  const sticky = /foo/y;
  re.lastIndex = 2;
  sticky.lastIndex = 2;
  const match = /(?<token>foo)/.exec("foo");
  const alias = { re, sticky, match, groups: match.groups };
  re.self = re;
  match.groups.owner = match;
  await wait("regexp");

  expect(alias.re).toBe(re);
  expect(re.self).toBe(re);
  expect(Object.getPrototypeOf(re)).toBe(RegExp.prototype);
  expect(alias.sticky).toBe(sticky);
  expect(alias.match).toBe(match);
  expect(alias.groups).toBe(match.groups);
  expect(match.groups.token).toBe("foo");
  expect(match.groups.owner).toBe(match);
  expect(re.lastIndex).toBe(2);
  expect(re.exec("fofoo").index).toBe(2);
  expect(re.lastIndex).toBe(5);
  expect(sticky.lastIndex).toBe(2);
  expect(sticky.exec("--foo").index).toBe(2);
  expect(sticky.lastIndex).toBe(5);
}
