// @returns undefined

import { expect } from 'test';

export function run() {
  const prototype = String.prototype;
  expect(prototype.trimLeft === prototype.trimStart).toBe(true);
  expect(prototype.trimRight === prototype.trimEnd).toBe(true);
  expect(prototype.trimLeft.name).toBe('trimStart');
  expect(' x '.trimRight()).toBe(' x');
  const descriptor = Object.getOwnPropertyDescriptor(prototype, 'trimLeft');
  expect(descriptor.writable && descriptor.configurable && !descriptor.enumerable).toBe(true);
  expect(prototype.substr.length).toBe(2);
  expect('abc'.substr(-2)).toBe('bc');
  expect('abc'.substr(-Infinity, Infinity)).toBe('abc');
  expect('abc'.substr(1, -1)).toBe('');
  expect('abc'.substr(1.9, 1.9)).toBe('b');
  expect('a😀z'.substr(1, 1)).toBe('\uD83D');
  let thrown;
  try { ''.substr(Infinity, { valueOf() { throw 'length'; } }); }
  catch (error) { thrown = error; }
  expect(thrown).toBe('length');

  expect('x'.anchor('a"&"')).toBe('<a name="a&quot;&&quot;">x</a>');
  expect('x'.link()).toBe('<a href="undefined">x</a>');
  expect('x'.fontcolor(null)).toBe('<font color="null">x</font>');
  expect('x'.fontsize(2)).toBe('<font size="2">x</font>');
  expect('x'.big()).toBe('<big>x</big>');
  expect('x'.blink()).toBe('<blink>x</blink>');
  expect('<x>\uD800'.bold(Symbol())).toBe('<b><x>\uD800</b>');
  expect('x'.fixed()).toBe('<tt>x</tt>');
  expect('x'.italics()).toBe('<i>x</i>');
  expect('x'.small()).toBe('<small>x</small>');
  expect('x'.strike()).toBe('<strike>x</strike>');
  expect('x'.sub()).toBe('<sub>x</sub>');
  expect('x'.sup()).toBe('<sup>x</sup>');
  let order = '';
  const receiver = { toString() { order += 'r'; return 'text'; } };
  const url = { toString() { order += 'u'; return 'url'; } };
  expect(prototype.link.call(receiver, url)).toBe('<a href="url">text</a>');
  expect(order).toBe('ru');
}
