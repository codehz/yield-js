// @returns "ok"

export function run() {
  const global = "a1b2c3".match(/\d/g);
  if (global.join(",") !== "1,2,3") return "match global";

  const single = "x-42-y".match(/(\d+)/);
  if (single[0] !== "42" || single.index !== 2 || single[1] !== "42") return "match single";
  if (single.input !== "x-42-y") return "match input";
  if ("abc".match(/z/) !== null) return "match null";
  if ("a1b2".match("\\d").index !== 1) return "match string";

  const empty = "ab".match(/(?:)/g);
  if (empty.length !== 3 || empty.join("|") !== "||") return "match empty";

  if ("abc123".search(/\d/) !== 3) return "search digit";
  if ("abc".search(/z/) !== -1) return "search miss";
  const stateful = /1/g;
  stateful.lastIndex = 2;
  if ("a1".search(stateful) !== 1 || stateful.lastIndex !== 2) return "search lastIndex";

  if ("a1b2".replace(/\d/g, "#") !== "a#b#") return "replace global";
  if ("a1b2".replace(/\d/, "#") !== "a#b2") return "replace first";
  if ("a-b".replace(/(\w)-(\w)/, "$2-$1") !== "b-a") return "replace swap";
  if ("2024-05".replace(/(?<y>\d+)-(?<m>\d+)/, "$<m>/$<y>") !== "05/2024") return "replace named";
  if ("a".replace(/a/, "$$") !== "$") return "replace dollar";
  if ("abc".replace(/b/, "($&)") !== "a(b)c") return "replace match";
  if ("abc".replace(/b/, "$`") !== "aac") return "replace prefix";
  if ("abc".replace(/b/, "$'") !== "acc") return "replace suffix";
  if ("abc".replace(/b/, (match, offset) => match.toUpperCase() + offset) !== "aB1c") return "replace fn";
  if ("a.b.c".replaceAll(".", "-") !== "a-b-c") return "replaceAll string";
  if ("a1b2".replaceAll(/\d/g, "#") !== "a#b#") return "replaceAll regexp";
  if ("ab".replaceAll("", "-") !== "-a-b-") return "replaceAll empty";
  try {
    "a1".replaceAll(/\d/, "#");
    return "replaceAll non-global";
  } catch (error) {
    if (!(error instanceof TypeError)) return "replaceAll error kind";
  }

  if ("a1b2c".split(/\d/).join(",") !== "a,b,c") return "split digits";
  if ("a1b2c".split(/(\d)/).join(",") !== "a,1,b,2,c") return "split captures";
  if ("a, b,c".split(/,\s*/).join(",") !== "a,b,c") return "split space";
  if ("abc".split(/(?:)/).join(",") !== "a,b,c") return "split empty";
  if ("".split(/x/).join(",") !== "")
    return "split empty input";
  if ("a1b".split(/\d/, 2).join(",") !== "a,b") return "split limit";
  if ("ab".split(/b/y).join(",") !== "a,") return "split sticky";

  const matches = [..."a1b2".matchAll(/(\d)/g)];
  if (matches.map((match) => match[0] + ":" + match[1]).join(",") !== "1:1,2:2")
    return "matchAll";
  const preserved = /\d/g;
  preserved.lastIndex = 9;
  const iterator = "a1".matchAll(preserved);
  if (preserved.lastIndex !== 9) return "matchAll lastIndex";
  iterator.next();
  if (iterator.next().done !== true) return "matchAll done";
  try {
    "a1".matchAll(/\d/);
    return "matchAll non-global";
  } catch (error) {
    if (!(error instanceof TypeError)) return "matchAll error kind";
  }

  if (/a/g[Symbol.match]("aa").length !== 2) return "symbol match";
  if (/a/[Symbol.search]("ba") !== 1) return "symbol search";
  if (/a/g[Symbol.replace]("aa", "b") !== "bb") return "symbol replace";
  if (/a/g[Symbol.split]("1a2").join(",") !== "1,2") return "symbol split";

  const customReplace = {};
  customReplace[Symbol.replace] = (string, replacement) => string + replacement;
  if ("abc".replace(customReplace, "!") !== "abc!") return "custom replace";

  const customSearch = {};
  customSearch[Symbol.search] = (string) => string.length;
  if ("abcd".search(customSearch) !== 4) return "custom search";

  if ("A1a2".matchAll(/a/gi).next().value[0] !== "A") return "matchAll flags";
  if ("ab".replaceAll(/(?:)/g, "-") !== "-a-b-") return "replaceAll empty global";

  let seenGroups = "unset";
  "a1".replace(/(\d)/, (match, digit, position, input, groups) => {
    seenGroups = groups;
    return "";
  });
  if (seenGroups !== undefined) return "replace groups argument";

  let namedDigit = "";
  "a1".replace(/(?<digit>\d)/, (match, digit, position, input, groups) => {
    namedDigit = groups.digit;
    return "";
  });
  if (namedDigit !== "1") return "replace named groups argument";

  const indexed = /\d/g;
  indexed.lastIndex = 7;
  "a1".replace(indexed, "#");
  if (indexed.lastIndex !== 0) return "replace lastIndex";

  return "ok";
}
