// @pending ["wait:regexp"]
// @restore
// @fulfill ["wait:regexp", null]
// @fulfilled true

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

  return alias.re === re && re.self === re && Object.getPrototypeOf(re) === RegExp.prototype &&
    alias.sticky === sticky && alias.match === match && alias.groups === match.groups &&
    match.groups.token === "foo" && match.groups.owner === match &&
    re.lastIndex === 2 && re.exec("fofoo").index === 2 && re.lastIndex === 5 &&
    sticky.lastIndex === 2 && sticky.exec("--foo").index === 2 && sticky.lastIndex === 5;
}
