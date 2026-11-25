import * as dom from 'yield:dom';

const htmlNamespace = 'http://www.w3.org/1999/xhtml';
const wrappers = new Map();
const handles = new WeakMap();

function wrap(handle) {
  if (handle === null) return null;
  const cached = wrappers.get(handle);
  if (cached !== undefined) return cached;

  const type = dom.kind(handle);
  const node = type === 1
    ? (dom.name(handle) === 'iframe' ? new HTMLIFrameElement() : new Element())
    : new Node();
  handles.set(node, handle);
  wrappers.set(handle, node);
  return node;
}

class Node extends EventTarget {
  get nodeType() { return dom.kind(handles.get(this)); }
  get nodeName() {
    const name = dom.name(handles.get(this));
    return this.nodeType === 1 ? name.toUpperCase() : name;
  }
  get firstChild() { return wrap(dom.first(handles.get(this))); }
  get nextSibling() { return wrap(dom.next(handles.get(this))); }
  get parentNode() { return wrap(dom.parent(handles.get(this))); }
  get ownerDocument() { return this === document ? null : document; }
  get textContent() { return dom.readText(handles.get(this)); }
  set textContent(value) {
    if (this !== document) dom.setText(handles.get(this), value === null ? '' : String(value));
  }
  appendChild(child) {
    dom.append(handles.get(this), handles.get(child));
    return child;
  }
}

class Element extends Node {
  style = {};
  get tagName() { return this.nodeName; }
  get namespaceURI() { return htmlNamespace; }
  setAttribute(name, value) {
    dom.setAttr(handles.get(this), String(name), String(value));
  }
}

export class HTMLIFrameElement extends Element {}

class Document extends Node {
  get defaultView() { return window; }
  get documentElement() { return wrap(dom.documentElement()); }
  get body() { return wrap(dom.body()); }
  get activeElement() { return this.body; }
  getElementById(id) { return wrap(dom.byId(String(id))); }
  createElement(tag) { return wrap(dom.element(String(tag))); }
  createTextNode(value) { return wrap(dom.text(String(value))); }
}

export const window = globalThis;
export const document = new Document();
const documentHandle = dom.parent(dom.documentElement());
handles.set(document, documentHandle);
wrappers.set(documentHandle, document);

// React links its stylesheet-resource path even when no stylesheet is mounted.
export const matchMedia = undefined;
