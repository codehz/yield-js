// @returns true

export function run() {
  const global = /foo/g;
  const first = global.exec("-foo-foo");
  if (first.index !== 1 || global.lastIndex !== 4) return false;
  if (!global.test("-foo-foo") || global.lastIndex !== 8) return false;
  if (global.exec("-foo-foo") !== null || global.lastIndex !== 0) return false;

  const sticky = /foo/y;
  if (sticky.test("-foo") || sticky.lastIndex !== 0) return false;
  sticky.lastIndex = 1;
  if (sticky.exec("-foo").index !== 1 || sticky.lastIndex !== 4) return false;
  if (sticky.test("-foo") || sticky.lastIndex !== 0) return false;

  const empty = /a*/g;
  const zero = empty.exec("b");
  if (zero[0] !== "" || zero.index !== 0 || empty.lastIndex !== 1) return false;

  const utf16 = /foo/g;
  if (utf16.exec("😀foo").index !== 2 || utf16.lastIndex !== 5) return false;

  const plain = /foo/;
  const desc = Object.getOwnPropertyDescriptor(plain, "lastIndex");
  plain.lastIndex = 99;
  return desc.value === 0 && desc.writable && !desc.enumerable && !desc.configurable &&
    plain.exec("-foo").index === 1 && plain.lastIndex === 99 &&
    !plain.test("other") && plain.lastIndex === 99;
}
