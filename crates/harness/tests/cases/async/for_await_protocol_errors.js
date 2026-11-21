// @fulfilled "TypeError|TypeError|TypeError|TypeError"
export async function run() {
  const names = [];

  const badIterator = { [Symbol.asyncIterator]() { return Promise.resolve(1); } };
  try { for await (const value of badIterator) {} } catch (error) { names.push(error.name); }

  const badStep = {
    [Symbol.asyncIterator]() { return this; },
    next() { return Promise.resolve(2); },
  };
  try { for await (const value of badStep) {} } catch (error) { names.push(error.name); }

  const badNext = { [Symbol.asyncIterator]() { return this; }, next: 1 };
  try { for await (const value of badNext) {} } catch (error) { names.push(error.name); }

  try { for await (const value of null) {} } catch (error) { names.push(error.name); }

  return names.join("|");
}
