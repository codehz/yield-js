import { Node, Document, Element, HTMLIFrameElement, openDocument } from 'yield:dom';

export { HTMLIFrameElement };
export const window = globalThis;
export const document = openDocument(window);

const setAttributeNative = Element.prototype.setAttribute;
const getElementByIdNative = Document.prototype.getElementById;
const createElementNative = Document.prototype.createElement;
const createTextNodeNative = Document.prototype.createTextNode;
const textContent = Object.getOwnPropertyDescriptor(Node.prototype, 'textContent');
const nodeType = Object.getOwnPropertyDescriptor(Node.prototype, 'nodeType').get;

// String conversion can run script, so perform it before entering the host call.
Object.defineProperty(Element.prototype, 'setAttribute', {
  value: function setAttribute(name, value) {
    return setAttributeNative.call(this, String(name), String(value));
  },
});
Object.defineProperty(Document.prototype, 'getElementById', {
  value: function getElementById(id) { return getElementByIdNative.call(this, String(id)); },
});
Object.defineProperty(Document.prototype, 'createElement', {
  value: function createElement(tag) { return createElementNative.call(this, String(tag)); },
});
Object.defineProperty(Document.prototype, 'createTextNode', {
  value: function createTextNode(value) { return createTextNodeNative.call(this, String(value)); },
});
Object.defineProperty(Node.prototype, 'textContent', {
  enumerable: false,
  set(value) {
    if (this === document) return;
    textContent.set.call(this, value === null ? '' : String(value));
  },
});

// React updates existing text nodes through nodeValue during commits.
Object.defineProperty(Node.prototype, 'nodeValue', {
  configurable: true,
  get() {
    const type = nodeType.call(this);
    return type === 3 || type === 8 ? textContent.get.call(this) : null;
  },
  set(value) {
    const type = nodeType.call(this);
    if (type === 3 || type === 8) {
      textContent.set.call(this, value === null ? '' : String(value));
    }
  },
});

// React links its stylesheet-resource path even when no stylesheet is mounted.
export const matchMedia = undefined;
