// @pending ["wait:proxy"]
// @restore
// @fulfill ["wait:proxy", 0]
// @pending ["wait:again"]
// @restore
// @fulfill ["wait:again", 0]
// @fulfilled true

function make() {
  let count = 0;
  const target = { value: 7 };
  const handler = {
    ownKeys() { throw "snapshot must not enumerate proxy"; },
    get(t, k, receiver) {
      if (this !== handler || t !== target || receiver !== proxy) throw "identity lost";
      if (k === "count") return count;
      count += 1;
      return t[k] + count;
    },
  };
  const proxy = new Proxy(target, handler);
  return { proxy, alias: proxy, target };
}

export async function run() {
  const saved = make();
  if (saved.proxy.value !== 8) return false;
  await wait("proxy");
  if (saved.alias !== saved.proxy || saved.alias.count !== 1) return false;
  saved.target.value = 10;
  if (saved.alias.value !== 12) return false;
  await wait("again");
  return saved.alias === saved.proxy && saved.proxy.count === 2 && saved.proxy.value === 13;
}
