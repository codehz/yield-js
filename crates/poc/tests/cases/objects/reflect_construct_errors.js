// @returns true

function Target() {}
async function asyncTarget() {}

export function run() {
  const arrow = () => 0;
  const method = {method() {}}.method;
  const invalid = [arrow, method, asyncTarget, arrow.bind(null), method.bind(null), asyncTarget.bind(null), String, {}];
  let touched = false;
  const list = {get length() { touched = true; return 0; }};
  let caught = 0;
  for (const value of invalid) {
    try { Reflect.construct(value, list); } catch (e) { if (e instanceof TypeError) caught++; }
    try { Reflect.construct(Target, list, value); } catch (e) { if (e instanceof TypeError) caught++; }
    try { new value(); } catch (e) { if (e instanceof TypeError) caught++; }
  }
  try { Reflect.construct(Target, list, undefined); } catch (e) { if (e instanceof TypeError) caught++; }
  try { Reflect.construct(Target, null); } catch (e) { if (e instanceof TypeError) caught++; }
  const thrown = {};
  function Throws() { throw thrown; }
  try { Reflect.construct(Throws, []); } catch (e) { if (e === thrown) caught++; }
  return !touched && caught === invalid.length * 3 + 3;
}
