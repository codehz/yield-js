// @returns true

export function run() {
  // All four ECMAScript line terminators are independent, even CR followed by LF.
  const lines = /^.$/gm;
  const a = lines.exec("a\rb\u2028c\u2029d\r\ne");
  const b = lines.exec(a.input);
  const c = lines.exec(a.input);
  const d = lines.exec(a.input);
  const e = lines.exec(a.input);
  return a.index === 0 && b.index === 2 && c.index === 4 &&
    d.index === 6 && e.index === 9 &&
    !/a$/.test("a\n") && !/[a-z]/i.test("ſK") &&
    !/\d/.test("١") && /\s/.test("\uFEFF") && !/\s/.test("\u0085") &&
    /\bcat\b/.test("écaté") && !/[]/.test("") && /[^]/.test("\n") &&
    /\uD83D\uDE00/.test("😀");
}
