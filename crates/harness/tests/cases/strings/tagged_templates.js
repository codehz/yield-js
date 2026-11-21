// @returns undefined

import { expect } from 'test';

function capture(strings, ...values) {
  return { strings, values };
}

export function run() {
  let first = capture`a${1}b${2}c`;
  let second = capture`a${1}b${2}c`;

  expect(Array.isArray(first.strings)).toBe(true);
  expect(first.strings.length).toBe(3);
  expect(first.strings[0]).toBe("a");
  expect(first.strings[1]).toBe("b");
  expect(first.strings[2]).toBe("c");
  expect(first.values.length).toBe(2);
  expect(first.values[0]).toBe(1);
  expect(first.values[1]).toBe(2);

  // Two source sites never share one template object, even with equal text.
  expect(first.strings === second.strings).toBe(false);

  // Every evaluation of the same site returns the same template object.
  function site() {
    return capture`x`;
  }
  expect(site().strings === site().strings).toBe(true);

  // The cooked and raw arrays are frozen, and raw has fixed attributes.
  expect(Object.isFrozen(first.strings)).toBe(true);
  expect(Object.isFrozen(first.strings.raw)).toBe(true);
  expect(first.strings.raw.length).toBe(3);
  expect(first.strings.raw[0]).toBe("a");
  expect(first.strings.raw[1]).toBe("b");
  expect(first.strings.raw[2]).toBe("c");
  let descriptor = Object.getOwnPropertyDescriptor(first.strings, "raw");
  expect(descriptor.value).toBe(first.strings.raw);
  expect(descriptor.writable).toBe(false);
  expect(descriptor.enumerable).toBe(false);
  expect(descriptor.configurable).toBe(false);
  let froze = false;
  try {
    first.strings[0] = "z";
  } catch (error) {
    froze = error instanceof TypeError;
  }
  expect(froze).toBe(true);

  // Substitutions are passed through without string conversion.
  let object = {};
  expect(capture`${object}`.values[0]).toBe(object);
  expect(capture`${undefined}`.values[0]).toBe(undefined);

  // Raw text keeps escapes, cooked text interprets them.
  let escaped = capture`\n${0}\u0041`;
  expect(escaped.strings[0]).toBe("\n");
  expect(escaped.strings[1]).toBe("A");
  expect(escaped.strings.raw[0]).toBe("\\n");
  expect(escaped.strings.raw[1]).toBe("\\u0041");

  // An invalid escape leaves cooked undefined but preserves raw text.
  let invalid = capture`\unicode${1}`;
  expect(invalid.strings[0]).toBe(undefined);
  expect(invalid.strings.raw[0]).toBe("\\unicode");

  // Tag reference evaluation precedes substitutions; the call comes last.
  let order = "";
  function note(value) {
    order += value;
    return value;
  }
  let holder = {
    get tag() {
      order += "tag";
      return function (strings) {
        order += "call";
        return strings;
      };
    },
  };
  holder.tag`${note("1")}${note("2")}`;
  expect(order).toBe("tag12call");

  // A member tag receives its base object as the receiver.
  let receiver;
  let method = {
    tag(strings) {
      receiver = this;
      return strings;
    },
  };
  method.tag`value`;
  expect(receiver).toBe(method);

  // Parentheses do not drop the member receiver; a call result can be a tag.
  let parenReceiver;
  let paren = {
    tag(strings) {
      parenReceiver = this;
      return strings;
    },
  };
  (paren.tag)`value`;
  expect(parenReceiver).toBe(paren);
  function makeTag() {
    return function (strings) {
      return strings;
    };
  }
  expect(makeTag()`x`[0]).toBe("x");

  // A super property tag keeps the derived instance as the receiver.
  class Base {
    tag(strings) {
      return this;
    }
  }
  class Derived extends Base {
    run() {
      return super.tag`value`;
    }
  }
  let derived = new Derived();
  expect(derived.run()).toBe(derived);

  // The template object feeds the existing String.raw implementation.
  expect(String.raw`a\nb${1}c`).toBe("a\\nb1c");
  expect(String.raw`${"x"}`).toBe("x");
}
