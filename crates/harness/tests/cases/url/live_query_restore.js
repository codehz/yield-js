// @pending ["wait:url"]
// @restore
// @fulfill ["wait:url", null]
// @fulfilled "https://xn--bcher-kva.example/c?q=a+b&new=%E2%98%83#frag"

import { expect, gc, wait } from "test";

export async function run() {
  const url = new URL("../c?first=1", "https://bücher.example:443/a/b");
  const params = url.searchParams;
  const keys = params.keys();
  expect(keys.next().value).toBe("first");
  url.search = "?first=2&second=3";
  expect(url.searchParams).toBe(params);
  await wait("url");
  gc();
  expect(keys.next().value).toBe("second");
  expect(params.get("first")).toBe("2");
  url.href = "https://bücher.example/c?q=a%20b";
  expect(params.get("q")).toBe("a b");
  params.append("new", "☃");
  url.hash = "frag";
  expect(URL.canParse("relative")).toBe(false);
  expect(URL.parse("relative")).toBe(null);
  expect(URL.parse("next", url).pathname).toBe("/next");
  return url.toJSON();
}
