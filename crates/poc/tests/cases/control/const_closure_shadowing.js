// @returns true

const moduleValue = 2;

function writeModuleValue() {
  moduleValue = 3;
}

export function run() {
  const value = 4;
  const writeOuter = () => { value = 5; };
  {
    let value = 6;
    const writeInner = () => { value = 7; };
    writeInner();
    if (value !== 7) return false;
  }
  try { writeOuter(); return false; }
  catch (error) { if (!(error instanceof TypeError)) return false; }
  try { writeModuleValue(); return false; }
  catch (error) { if (!(error instanceof TypeError)) return false; }
  return value === 4 && moduleValue === 2;
}
