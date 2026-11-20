// @returns true

export function run() {
  const match = /./.exec("😀");
  return match !== null && match[0].length === 1 && match[0].charCodeAt(0) === 0xD83D;
}
