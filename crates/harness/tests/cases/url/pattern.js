// @pending ["wait:pattern"]
// @restore
// @fulfill ["wait:pattern", true]
// @fulfilled true

import { wait } from "test";

function assert(condition) {
  if (!condition) throw new Error("URLPattern assertion failed");
}

export async function run() {
  const base = "https://example.com/a/";
  const pattern = new URLPattern("../items/:id([0-9]+){/:label}?", base, { ignoreCase: true });
  const dictionary = new URLPattern({ pathname: "/post/:name" });
  const marker = {};
  pattern.marker = marker;
  await wait("pattern");

  assert(pattern.marker === marker);
  assert(Object.prototype.toString.call(pattern) === "[object URLPattern]");
  assert(pattern.protocol === "https" && pattern.hostname === "example.com");
  assert(pattern.pathname === "/items/:id([0-9]+)/:label?");
  assert(pattern.hasRegExpGroups && !dictionary.hasRegExpGroups);
  assert(!pattern.test("https://example.com/items/nope"));
  assert(!pattern.test("invalid URL"));
  assert(pattern.exec("https://other.example/items/13") === null);
  const matched = pattern.exec("../ITEMS/13", base);
  assert(matched.inputs[0] === "../ITEMS/13" && matched.inputs[1] === base);
  assert(matched.pathname.input === "/ITEMS/13");
  assert(matched.pathname.groups.id === "13");
  assert(Object.hasOwn(matched.pathname.groups, "label"));
  assert(matched.pathname.groups.label === undefined);

  const input = { pathname: "/post/welcome", ignored: true };
  const result = dictionary.exec(input);
  assert(result.pathname.groups.name === "welcome");
  assert(result.inputs[0].pathname === input.pathname);
  assert(result.inputs[0] !== input && !Object.hasOwn(result.inputs[0], "ignored"));
  assert(new URLPattern().test());
  assert(new URLPattern({ pathname: "/x/:id((?=a)a)" }).test({ pathname: "/x/a" }));
  assert(new URLPattern({ pathname: "/Hello" }, { ignoreCase: true }).test({ pathname: "/hello" }));

  try { URLPattern(); assert(false); } catch (error) { assert(error instanceof TypeError); }
  try { new URLPattern("/relative"); assert(false); } catch (error) { assert(error instanceof TypeError); }
  try { dictionary.test(input, base); assert(false); } catch (error) { assert(error instanceof TypeError); }
  try { URLPattern.prototype.exec.call({}); assert(false); } catch (error) { assert(error instanceof TypeError); }
  try {
    dictionary.test({ get pathname() { throw marker; } });
    assert(false);
  } catch (error) {
    assert(error === marker);
  }
  return true;
}
