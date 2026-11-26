import {
  Node, Document, Element, HTMLElement, HTMLIFrameElement, HTMLInputElement,
  HTMLButtonElement, HTMLLabelElement, HTMLUListElement, HTMLLIElement, openDocument,
} from 'yield:dom';

export {
  HTMLElement, HTMLIFrameElement, HTMLInputElement, HTMLButtonElement,
  HTMLLabelElement, HTMLUListElement, HTMLLIElement,
};
export {
  HTMLDivElement, HTMLSpanElement, HTMLHeadingElement, HTMLParagraphElement,
  HTMLOListElement, HTMLQuoteElement, HTMLPreElement, HTMLBRElement, HTMLHRElement,
} from 'yield:dom';
export const window = globalThis;
export const document = openDocument(window);

// Presence advertises native input event support to event consumers.
Object.defineProperty(document, 'oninput', { value: null, configurable: true });

const setAttributeNative = Element.prototype.setAttribute;
const getAttributeNative = Element.prototype.getAttribute;
const removeAttributeNative = Element.prototype.removeAttribute;
const inputValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value');
const inputDefaultValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'defaultValue');
const inputChecked = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'checked');
const inputDefaultChecked = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'defaultChecked');
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
Object.defineProperty(Element.prototype, 'getAttribute', {
  value: function getAttribute(name) { return getAttributeNative.call(this, String(name)); },
});
Object.defineProperty(Element.prototype, 'removeAttribute', {
  value: function removeAttribute(name) { return removeAttributeNative.call(this, String(name)); },
});
function stringAttribute(name) {
  return {
    configurable: true,
    get() { return getAttributeNative.call(this, name) ?? ''; },
    set(value) { setAttributeNative.call(this, name, String(value)); },
  };
}

function booleanAttribute(name) {
  return {
    configurable: true,
    get() { return getAttributeNative.call(this, name) !== null; },
    set(value) {
      if (value) setAttributeNative.call(this, name, '');
      else removeAttributeNative.call(this, name);
    },
  };
}

Object.defineProperties(HTMLElement.prototype, {
  id: stringAttribute('id'),
  className: stringAttribute('class'),
});
Object.defineProperty(HTMLLabelElement.prototype, 'htmlFor', stringAttribute('for'));
Object.defineProperty(HTMLButtonElement.prototype, 'disabled', booleanAttribute('disabled'));

Object.defineProperties(HTMLInputElement.prototype, {
  value: { set(value) { inputValue.set.call(this, value === null ? '' : String(value)); } },
  defaultValue: { set(value) { inputDefaultValue.set.call(this, String(value)); } },
  checked: { set(value) { inputChecked.set.call(this, Boolean(value)); } },
  defaultChecked: { set(value) { inputDefaultChecked.set.call(this, Boolean(value)); } },
  type: {
    get() { return getAttributeNative.call(this, 'type') || 'text'; },
    set(value) { setAttributeNative.call(this, 'type', String(value)); },
  },
  name: {
    get() { return getAttributeNative.call(this, 'name') || ''; },
    set(value) { setAttributeNative.call(this, 'name', String(value)); },
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
