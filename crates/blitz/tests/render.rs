use blitz_dom::{Document, NodeData, NodeId};
use blitz_traits::events::{
    BlitzPointerEvent, BlitzPointerId, DomEvent, DomEventData, EventDispatchSource, PointerCoords,
    UiEvent,
};
use yield_blitz::{BlitzSession, compile_fixture, compile_module, render_module};
use yield_vm::{Build, BuildResource, RestoreOptions, Snapshot, SnapshotValidation, Value};

#[test]
fn restores_react_author_styles_in_import_order() -> Result<(), String> {
    let build = compile_fixture()?;
    let [first, second] = build.resources.as_slice() else {
        panic!("expected two reachable stylesheets")
    };
    assert_eq!(first.uri, "asset:src/styles.css");
    assert_eq!(second.uri, "asset:src/cascade.css");
    let expected = [
        include_str!("../../../fixtures/npm-react-app/src/styles.css"),
        include_str!("../../../fixtures/npm-react-app/src/cascade.css"),
    ];
    assert_eq!(&*first.bytes, expected[0].as_bytes());
    assert_eq!(&*second.bytes, expected[1].as_bytes());

    let check_layout = |session: &mut BlitzSession| {
        session.resolve_layout(0.0);
        let doc = session.document();
        let root = doc
            .get_node(doc.get_element_by_id("root").unwrap())
            .unwrap();
        assert_eq!(root.final_layout().size.width, 137.0);
        assert_eq!(style_texts(&doc), expected);
        assert_eq!(doc.author_stylesheets().count(), 2);
    };
    let artifact = build.to_bytes();
    let mut session = BlitzSession::new(Build::from_bytes(&artifact)?)?;
    session.run_until_idle()?;
    check_layout(&mut session);
    let snapshot = session.snapshot(&[])?.to_bytes()?;
    drop(session);

    let mut restored = BlitzSession::restore(
        Snapshot::from_bytes(&snapshot)?,
        Build::from_bytes(&artifact)?,
        RestoreOptions::default(),
    )?;
    check_layout(&mut restored);
    Ok(())
}

#[test]
fn rejects_unknown_and_invalid_css_without_mutating_head() -> Result<(), String> {
    let mut build = compile_module(
        "import { insertCss } from 'yield:dom';\n\
         export function check(uri) {\n\
           try { insertCss(uri); } catch (error) {\n\
             return error instanceof TypeError ? error.message : 'not a TypeError';\n\
           }\n\
         }",
    )?;
    build.resources.push(BuildResource {
        uri: "asset:opaque".into(),
        bytes: [0x00, 0xff, 0x80].into(),
    });
    build.finalize();
    let mut session = BlitzSession::new(Build::from_bytes(&build.to_bytes())?)?;
    for (uri, expected) in [
        (
            "asset:missing.css",
            "unknown CSS resource: asset:missing.css",
        ),
        ("asset:opaque", "invalid UTF-8 CSS resource: asset:opaque"),
    ] {
        let result = session.start("check", vec![yield_vm::JsValue::string(uri)])?;
        assert_eq!(result.into_string().unwrap().to_utf8().unwrap(), expected);
        assert!(style_texts(&session.document()).is_empty());
    }
    Ok(())
}

fn style_texts(doc: &blitz_dom::BaseDocument) -> Vec<String> {
    let head = doc
        .root_element()
        .children
        .iter()
        .filter_map(|&id| doc.get_node(id))
        .find(|node| {
            node.element_data()
                .is_some_and(|el| &*el.name.local == "head")
        })
        .unwrap();
    head.children
        .iter()
        .filter_map(|&id| doc.get_node(id))
        .filter(|node| {
            node.element_data()
                .is_some_and(|el| &*el.name.local == "style")
        })
        .map(|node| node.text_content())
        .collect()
}

#[test]
fn mounts_react_and_resumes_the_click_counter() -> Result<(), String> {
    let build = compile_fixture()?;
    let mut session = BlitzSession::new(build.clone())?;
    session.run_until_idle()?;
    let doc = session.document();
    let root = doc
        .get_node(doc.get_element_by_id("root").unwrap())
        .unwrap();
    let [main] = root.children.as_slice() else {
        panic!("expected one child of #root")
    };
    let main = doc.get_node(*main).unwrap();
    assert_eq!(&*main.element_data().unwrap().name.local, "main");
    let [heading, counter] = main.children.as_slice() else {
        panic!("expected a heading and counter in main")
    };
    let heading = doc.get_node(*heading).unwrap();
    assert_eq!(&*heading.element_data().unwrap().name.local, "h1");
    assert_eq!(heading.text_content(), "Hello from React DOM");
    assert_eq!(doc.get_node(*counter).unwrap().text_content(), "Clicks: 0");
    let counter = *counter;
    drop(doc);
    deliver_blitz_click(&mut session, counter);
    let text = |session: &BlitzSession, id| session.document().get_node(id).unwrap().text_content();
    assert_eq!(text(&session, counter), "Clicks: 1");
    let bytes = session.snapshot(&[])?.to_bytes()?;
    drop(session);

    let mut restored = BlitzSession::restore(
        Snapshot::from_bytes(&bytes)?,
        build,
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )?;
    let counter = restored.document().get_element_by_id("counter").unwrap();
    assert_eq!(text(&restored, counter), "Clicks: 1");
    assert!(restored.click(counter)?);
    assert_eq!(text(&restored, counter), "Clicks: 2");
    Ok(())
}

// Bypass winit hit testing, but use the same document event driver as the window.
fn deliver_blitz_click(session: &mut BlitzSession, target: NodeId) {
    let pointer = BlitzPointerEvent {
        id: BlitzPointerId::Mouse,
        is_primary: true,
        coords: PointerCoords {
            page_x: 0.0,
            page_y: 0.0,
            screen_x: 0.0,
            screen_y: 0.0,
            client_x: 0.0,
            client_y: 0.0,
        },
        button: Default::default(),
        buttons: Default::default(),
        mods: Default::default(),
        details: Default::default(),
        element: Default::default(),
        active_pointers: Default::default(),
    };
    session.inner_mut().queue_event(DomEvent::new(
        target,
        DomEventData::Click(pointer.clone()),
        EventDispatchSource::UserAgent,
    ));
    session.handle_ui_event(UiEvent::PointerMove(pointer));
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
        check(frame.tagName === 'IFRAME' && frame.nodeType === 1, 'iframe inherited DOM accessors');
        frame.setAttribute('title', 'inherited DOM method');
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
fn restores_detached_nodes_and_resumes_script_updates() -> Result<(), String> {
    let build = compile_module(
        r#"
        const root = document.getElementById('root');
        let detached;
        let child;
        let events = 0;

        export function detach() {
            // Register the interior node before its parent in the wrapper table.
            child = document.createElement('span');
            detached = document.createElement('section');
            detached.setAttribute('id', 'detached');
            detached.style.color = 'red';
            child.textContent = 'unwrapped text';
            detached.appendChild(child);
            detached.addEventListener('test', () => { events++; });
            root.appendChild(detached);
            root.textContent = 'replacement';
        }

        export function update() {
            if (root.firstChild.textContent !== 'replacement' || detached.parentNode !== null ||
                document.getElementById('detached') !== null || child.parentNode !== detached ||
                detached.firstChild !== child || detached.style.color !== 'red' ||
                child.textContent !== 'unwrapped text') return false;

            root.appendChild(detached);
            detached.setAttribute('id', 'reattached');
            child.firstChild.textContent = 'updated';
            detached.dispatchEvent(new Event('test'));
            return document.getElementById('reattached') === detached &&
                root.firstChild.nextSibling === detached && detached.parentNode === root &&
                events === 1;
        }
        "#,
    )?;
    let bytes = {
        let mut session = BlitzSession::new(build.clone())?;
        session.start("detach", vec![])?;
        session.run_until_idle()?;
        session.snapshot(&[])?.to_bytes()?
    };

    let mut session = BlitzSession::restore(
        Snapshot::from_bytes(&bytes)?,
        build,
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )?;
    assert_eq!(session.start("update", vec![])?, Value::boolean(true));
    session.run_until_idle()?;
    let doc = session.document();
    let reattached = doc
        .get_node(doc.get_element_by_id("reattached").unwrap())
        .unwrap();
    assert_eq!(reattached.text_content(), "updated");
    assert_eq!(reattached.parent, doc.get_element_by_id("root"));
    Ok(())
}

#[test]
fn returns_uncaught_script_errors() {
    for (source, message) in [
        (
            "throw new Error('startup failed');",
            "script threw: Error: startup failed",
        ),
        (
            "queueMicrotask(() => { throw new TypeError('microtask failed'); });",
            "TypeError: microtask failed",
        ),
    ] {
        let Err(error) = render_module(source) else {
            panic!("expected error: {source}")
        };
        assert_eq!(error.matches(message).count(), 1, "{error}");
        assert!(error.contains("main.mjs:1:"), "{error}");
    }
}

#[test]
fn dispatches_click_after_snapshot_restore() -> Result<(), String> {
    let build = compile_module(
        r#"
        const button = document.createElement('button');
        button.setAttribute('id', 'button');
        let clicks = 0;
        const root = document.getElementById('root');
        root.addEventListener('click', event => {
            if (event.target !== button || event.currentTarget !== root || event.eventPhase !== 3) return;
            if (event.type !== 'click' || !event.bubbles || !event.cancelable || !event.isTrusted) return;
            event.preventDefault();
            queueMicrotask(() => { clicks++; });
        }, { once: true });
        root.appendChild(button);
        export function count() { return clicks; }
        "#,
    )?;
    let bytes = {
        let session = BlitzSession::new(build.clone())?;
        session.snapshot(&[])?.to_bytes()?
    };
    let mut session = BlitzSession::restore(
        Snapshot::from_bytes(&bytes)?,
        build,
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )?;
    let target = session.document().get_element_by_id("button").unwrap();
    assert!(!session.click(target)?);
    assert!(session.click(target)?);
    assert_eq!(session.start("count", vec![])?, Value::number(1.0));
    Ok(())
}
