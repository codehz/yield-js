use blitz_dom::NodeData;
use yield_blitz::{render_fixture, render_module};

#[test]
fn mounts_react_dom_into_blitz() -> Result<(), String> {
    let doc = render_fixture()?;
    let root = doc
        .get_node(doc.get_element_by_id("root").unwrap())
        .unwrap();
    let [main] = root.children.as_slice() else {
        panic!("expected one child of #root")
    };
    let main = doc.get_node(*main).unwrap();
    assert_eq!(&*main.element_data().unwrap().name.local, "main");
    let [heading] = main.children.as_slice() else {
        panic!("expected one child of main")
    };
    let heading = doc.get_node(*heading).unwrap();
    assert_eq!(&*heading.element_data().unwrap().name.local, "h1");
    assert_eq!(heading.text_content(), "Hello from React DOM");
    Ok(())
}

#[test]
fn keeps_native_node_identity_and_mutates_the_tree() -> Result<(), String> {
    let doc = render_module(
        r#"
        import { Node, Document, Element, openDocument } from 'yield:dom';

        function check(value, message) {
            if (!value) throw new Error(message);
        }
        function rejects(action) {
            try { action(); } catch (error) { return error instanceof TypeError; }
            return false;
        }

        const root = document.getElementById('root');
        const child = document.createElement('P');
        const frame = document.createElement('IFRAME');
        check(frame instanceof HTMLIFrameElement && frame instanceof Element &&
              frame instanceof Node && frame instanceof EventTarget, 'iframe class chain');
        check(child.style !== frame.style, 'style is per-instance');
        child.style.color = 'red';
        child.expando = frame;
        check(document instanceof Document && document.ownerDocument === null, 'document brand');
        check(openDocument({}) === document, 'document opens once');
        check(document === window.document && document.defaultView === window, 'default view');
        check(document.documentElement.parentNode === document, 'document identity');
        check(document.activeElement === document.body, 'focus fallback');
        check(child.namespaceURI === 'http://www.w3.org/1999/xhtml', 'namespace');
        check(child.tagName === 'P' && child.nodeName === 'P' && child.nodeType === 1, 'element kind');
        check(root.appendChild(child) === child && root.firstChild === child, 'node identity');
        check(child.parentNode === root && child.ownerDocument === document, 'tree links');
        child.setAttribute({ toString() { return 'ID'; } }, { toString() { return 'child'; } });
        const found = document.getElementById({ toString() { return 'child'; } });
        check(found === child && found.style.color === 'red' && found.expando === frame,
              'coerced ID lookup preserves fields');
        const conversionError = new Error('attribute name conversion');
        let failure;
        let valueConverted = false;
        const throwing = { toString() { throw conversionError; } };
        try {
            child.setAttribute(throwing, { toString() { valueConverted = true; return 'other'; } });
        } catch (error) { failure = error; }
        check(failure === conversionError && !valueConverted, 'name conversion precedes value');
        check(document.getElementById('child') === child, 'failed conversion leaves id intact');
        document.textContent = throwing;
        check(document.textContent === null && document.getElementById('root') === root,
              'document textContent skips conversion and leaves tree intact');
        check(document.createTextNode().textContent === 'undefined', 'missing value is stringified');
        check(document.createElement('straße').tagName === 'STRASSE', 'Unicode tag name');
        check(!Object.getOwnPropertyDescriptor(Element.prototype, 'setAttribute').enumerable &&
              !Object.getOwnPropertyDescriptor(Node.prototype, 'textContent').enumerable,
              'shim leaves prototype members non-enumerable');

        child.textContent = 'old';
        const old = child.firstChild;
        check(old.nodeType === 3 && old.nodeName === '#text', 'text kind');
        child.textContent = '';
        check(child.firstChild === null && old.parentNode === null, 'non-destructive detach');
        check(old.textContent === 'old', 'detached content survives');
        old.textContent = 'detached';
        check(old.textContent === 'detached', 'text mutation');
        child.textContent = 'new';
        check(child.firstChild !== old && child.firstChild.nextSibling === null, 'replacement');
        check(child.textContent === 'new', 'reads from native tree');
        check(rejects(() => old.appendChild(child)), 'text parent must be rejected');
        check(rejects(() => child.appendChild(root)), 'cycle must be rejected');
        check(rejects(() => child.appendChild(0)) && rejects(() => child.appendChild({})),
              'appendChild requires native nodes');
        check(rejects(() => child.appendChild(Object.create(Node.prototype))) &&
              rejects(() => child.appendChild(new Node())) &&
              rejects(() => new Document().createElement('p')), 'unbound nodes cannot access tree');

        let events = 0;
        const listener = () => { events++; };
        child.addEventListener('test', listener);
        child.dispatchEvent(new Event('test'));
        child.removeEventListener('test', listener);
        child.dispatchEvent(new Event('test'));
        check(events === 1, 'uses real EventTarget registration');
    "#,
    )?;
    let root = doc
        .get_node(doc.get_element_by_id("root").unwrap())
        .unwrap();
    let child = doc
        .get_node(doc.get_element_by_id("child").unwrap())
        .unwrap();
    assert_eq!(root.children.as_slice(), &[child.id]);
    let [text] = child.children.as_slice() else {
        panic!("expected a single text node after replacement")
    };
    assert!(
        matches!(&doc.get_node(*text).unwrap().data, NodeData::Text(text) if text.content == "new")
    );
    Ok(())
}

#[test]
fn returns_uncaught_script_errors() {
    for (source, message) in [
        (
            "throw new Error('startup failed');",
            "Error: startup failed",
        ),
        (
            "queueMicrotask(() => { throw new TypeError('microtask failed'); });",
            "TypeError: microtask failed",
        ),
    ] {
        let Err(error) = render_module(source) else {
            panic!("expected error: {source}")
        };
        assert!(error.contains(message), "{error}");
        assert!(error.contains("main.mjs:1:"), "{error}");
    }
}
