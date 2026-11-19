// @returns true

export function run() {
  let order = '';
  const thrown = {};
  const arrow = () => {};
  Object.setPrototypeOf(arrow, {
    get prototype() {
      order += 'g';
      if (this !== arrow) return {};
      throw thrown;
    }
  });
  const bound = arrow.bind(null);
  if (1 instanceof bound || order !== '') return false;

  function left() { order += 'l'; return {}; }
  function right() { order += 'r'; return bound; }
  try {
    return left() instanceof right();
  } catch (e) {
    return e === thrown && order === 'lrg';
  }
}
