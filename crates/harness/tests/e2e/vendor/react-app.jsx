import { render } from './renderer.mjs';

const container = { children: [] };

export function run() {
  render(<scene id="root">hello</scene>, container);
}

export function committed() {
  if (container.children.length !== 1) throw 'expected one committed root';
  const root = container.children[0];
  if (root.type !== 'scene' || root.props.id !== 'root') throw 'wrong root';
  if (root.children.length !== 1 || root.children[0].text !== 'hello') throw 'wrong child';
  return root.children.length;
}
