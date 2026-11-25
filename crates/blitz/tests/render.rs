use blitz_dom::NodeData;
use yield_blitz::render_module;

#[test]
fn mounts_react_dom_into_blitz() -> Result<(), String> {
    let doc = render_module(include_str!("../../../fixtures/npm-react-app/blitz.mjs"))?;
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
fn keeps_wrapper_identity_and_mutates_the_native_tree() -> Result<(), String> {
    let doc = render_module(
        r#"
        import { append, first } from 'yield:dom';

        function check(value, message) {
            if (!value) throw new Error(message);
        }
        function rejects(action) {
            try { action(); } catch (error) { return error instanceof TypeError; }
            return false;
        }

        const root = document.getElementById('root');
        const child = document.createElement('P');
        check(document === window.document && document.defaultView === window, 'default view');
        check(document.documentElement.parentNode === document, 'document identity');
        check(document.activeElement === document.body, 'focus fallback');
        check(child.namespaceURI === 'http://www.w3.org/1999/xhtml', 'namespace');
        check(child.tagName === 'P' && child.nodeName === 'P' && child.nodeType === 1, 'element kind');
        check(root.appendChild(child) === child && root.firstChild === child, 'wrapper identity');
        check(child.parentNode === root && child.ownerDocument === document, 'tree links');
        child.setAttribute('id', 'child');
        check(document.getElementById('child') === child, 'attribute updated native ID lookup');

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
        check(rejects(() => first(99999)) && rejects(() => append(NaN, 0)), 'invalid handles');

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
