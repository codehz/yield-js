use blitz_dom::{Document, NodeData, NodeId};
use blitz_traits::events::{
    BlitzImeEvent, BlitzPointerEvent, BlitzPointerId, DomEvent, DomEventData, EventDispatchSource,
    PointerCoords, UiEvent,
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
        assert_eq!(root.final_layout().size.width, 480.0);
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
fn operates_react_todos_before_and_after_restore() -> Result<(), String> {
    let build = compile_fixture()?;
    let mut session = BlitzSession::new(build.clone())?;
    session.run_until_idle()?;
    assert_todos(&session, &[], 0);

    commit_text(&mut session, "new-todo", "   ");
    assert_input_text(&session, "new-todo", "   ");
    click_id(&mut session, "add-todo");
    assert_todos(&session, &[], 0);

    commit_text(&mut session, "new-todo", "小明");
    click_id(&mut session, "add-todo");
    assert_todos(&session, &[(1, "小明", false)], 1);
    assert_input_text(&session, "new-todo", "");
    commit_text(&mut session, "new-todo", " Beta ");
    click_id(&mut session, "add-todo");
    assert_todos(&session, &[(1, "小明", false), (2, "Beta", false)], 2);
    assert_input_text(&session, "new-todo", "");

    click_id(&mut session, "toggle-1");
    assert_todos(&session, &[(1, "小明", true), (2, "Beta", false)], 1);
    click_id(&mut session, "filter-active");
    assert_todos(&session, &[(2, "Beta", false)], 1);
    click_id(&mut session, "filter-completed");
    assert_todos(&session, &[(1, "小明", true)], 1);

    let bytes = session.snapshot(&[])?.to_bytes()?;
    drop(session);
    let mut session = BlitzSession::restore(
        Snapshot::from_bytes(&bytes)?,
        build,
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )?;
    assert_todos(&session, &[(1, "小明", true)], 1);
    click_id(&mut session, "filter-all");
    assert_todos(&session, &[(1, "小明", true), (2, "Beta", false)], 1);

    click_id(&mut session, "edit-2");
    assert_input_text(&session, "draft-2", "Beta");
    commit_text(&mut session, "draft-2", "🙂");
    assert_input_text(&session, "draft-2", "Beta🙂");
    click_id(&mut session, "cancel-2");
    assert!(session.document().get_element_by_id("draft-2").is_none());
    assert_todos(&session, &[(1, "小明", true), (2, "Beta", false)], 1);
    click_id(&mut session, "edit-2");
    commit_text(&mut session, "draft-2", "🙂 ");
    click_id(&mut session, "save-2");
    assert!(session.document().get_element_by_id("draft-2").is_none());
    assert_todos(&session, &[(1, "小明", true), (2, "Beta🙂", false)], 1);

    click_id(&mut session, "filter-active");
    click_id(&mut session, "toggle-all");
    assert_todos(&session, &[], 0);
    assert_checkbox(&session, "toggle-all", true);
    click_id(&mut session, "filter-all");
    assert_todos(&session, &[(1, "小明", true), (2, "Beta🙂", true)], 0);
    click_id(&mut session, "toggle-all");
    assert_checkbox(&session, "toggle-all", false);
    assert_todos(&session, &[(1, "小明", false), (2, "Beta🙂", false)], 2);
    click_id(&mut session, "toggle-1");
    click_id(&mut session, "filter-active");
    click_id(&mut session, "clear-completed");
    click_id(&mut session, "filter-all");
    assert_todos(&session, &[(2, "Beta🙂", false)], 1);
    click_id(&mut session, "delete-2");
    assert_todos(&session, &[], 0);
    assert_checkbox(&session, "toggle-all", false);
    Ok(())
}

fn assert_todos(session: &BlitzSession, expected: &[(usize, &str, bool)], active: usize) {
    let doc = session.document();
    let list = doc
        .get_node(doc.get_element_by_id("todo-list").unwrap())
        .unwrap();
    assert_eq!(list.children.len(), expected.len());
    for (&row, &(id, title, checked)) in list.children.iter().zip(expected) {
        assert_eq!(doc.get_element_by_id(&format!("todo-{id}")), Some(row));
        let toggle = format!("toggle-{id}");
        let label = doc
            .get_node(row)
            .unwrap()
            .children
            .iter()
            .filter_map(|&child| doc.get_node(child))
            .flat_map(|node| &node.children)
            .filter_map(|&child| doc.get_node(child))
            .find(|node| node.attr(blitz_dom::local_name!("for")) == Some(&toggle))
            .unwrap();
        assert_eq!(label.text_content(), title);
        assert_checkbox(session, &toggle, checked);
    }
    let count = doc
        .get_node(doc.get_element_by_id("todo-count").unwrap())
        .unwrap();
    assert_eq!(count.text_content(), format!("{active} left"));
}

fn assert_checkbox(session: &BlitzSession, id: &str, checked: bool) {
    let doc = session.document();
    let element = doc
        .get_node(doc.get_element_by_id(id).unwrap())
        .unwrap()
        .element_data()
        .unwrap();
    assert_eq!(element.checkbox_input_checked(), Some(checked), "#{id}");
}

fn click_id(session: &mut BlitzSession, id: &str) {
    let target = session
        .document()
        .get_element_by_id(id)
        .unwrap_or_else(|| panic!("missing #{id}"));
    deliver_blitz_click(session, target);
}

#[test]
fn restores_text_edited_independently_of_the_value_attribute() -> Result<(), String> {
    let build = compile_module(
        r#"
        const input = document.createElement('input');
        input.setAttribute('id', 'name');
        input.defaultValue = 'seed';
        document.getElementById('root').appendChild(input);
        let edited = false;
        input.addEventListener('input', event => {
            if (document.activeElement !== input || event.target !== input ||
                !event.bubbles || !event.isTrusted || event.cancelable) {
                throw new Error('invalid native input event');
            }
            edited = true;
        });
        export function check() {
            if (!edited || !(input instanceof HTMLInputElement) || input.defaultValue !== 'seed' ||
                input.getAttribute('value') !== 'seed' || input.value !== 'seed!') {
                throw new Error('live text must not overwrite the default');
            }
        }
    "#,
    )?;
    let mut session = BlitzSession::new(build.clone())?;
    commit_text(&mut session, "name", "!");
    session.start("check", vec![])?;
    let snapshot = session.snapshot(&[])?;
    drop(session);

    let mut restored = BlitzSession::restore(snapshot, build, Default::default())?;
    restored.start("check", vec![])?;
    Ok(())
}

fn commit_text(session: &mut BlitzSession, id: &str, text: &str) {
    session.resolve_layout(0.0);
    let target = session.document().get_element_by_id(id).unwrap();
    let mut doc = session.inner_mut();
    doc.set_focus_to(target);
    assert_eq!(doc.get_focussed_node_id(), Some(target));
    doc.with_text_input(target, |mut driver| driver.move_to_text_end());
    drop(doc);
    // Let Blitz perform the edit and generate its own input event.
    session.handle_ui_event(UiEvent::Ime(BlitzImeEvent::Commit(text.into())));
}

fn assert_input_text(session: &BlitzSession, id: &str, expected: &str) {
    let doc = session.document();
    let input = doc.get_node(doc.get_element_by_id(id).unwrap()).unwrap();
    assert_eq!(
        input
            .element_data()
            .unwrap()
            .text_input_data()
            .unwrap()
            .editor
            .text()
            .to_string(),
        expected
    );
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
fn restores_html_wrappers_and_reflected_attributes() -> Result<(), String> {
    let build = compile_module(
        r#"
        const root = document.getElementById('root');
        const button = document.createElement('button');
        const label = document.createElement('label');
        const list = document.createElement('ul');
        const item = document.createElement('li');
        if (button.disabled || label.htmlFor !== '' || item.id !== '' || item.className !== '') {
            throw new Error('missing attributes must reflect as false or empty strings');
        }
        for (const [node, id] of [[button, 'action'], [label, 'label'], [list, 'list'], [item, 'item']]) {
            node.id = id;
            root.appendChild(node);
        }
        list.appendChild(item);
        label.htmlFor = { toString() { return 'action'; } };
        item.className = 'task';
        button.disabled = 'yes';
        if (button.getAttribute('disabled') !== '') throw new Error('boolean attribute not set');
        button.disabled = 0;
        if (button.getAttribute('disabled') !== null) throw new Error('boolean attribute not removed');
        button.setAttribute('disabled', 'false');

        export function check() {
            for (const [node, id, type] of [[button, 'action', HTMLButtonElement],
                [label, 'label', HTMLLabelElement], [list, 'list', HTMLUListElement],
                [item, 'item', HTMLLIElement]]) {
                if (Object.getPrototypeOf(node) !== type.prototype || !(node instanceof HTMLElement) ||
                    document.getElementById(id) !== node || document.getElementById(id) !== node) {
                    throw new Error('wrapper identity or prototype changed: ' + id);
                }
            }
            return root instanceof HTMLElement && list.firstChild === item && button.disabled &&
                button.id === 'action' && label.htmlFor === 'action' && label.getAttribute('for') === 'action' &&
                item.className === 'task' && item.getAttribute('class') === 'task';
        }
        "#,
    )?;
    let mut session = BlitzSession::new(build.clone())?;
    assert_eq!(session.start("check", vec![])?, Value::boolean(true));
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
    assert_eq!(restored.start("check", vec![])?, Value::boolean(true));
    Ok(())
}

#[test]
fn inserts_moves_and_removes_children_without_rewrapping() -> Result<(), String> {
    render_module(
        r#"
        import { Node } from 'yield:dom';
        const root = document.getElementById('root');
        const [a, b, c] = ['a', 'b', 'c'].map(text => {
            const item = document.createElement('div');
            item.textContent = text;
            return item;
        });
        function check(value, message) { if (!value) throw new Error(message); }
        function rejects(action) {
            try { action(); } catch (error) { return error instanceof TypeError; }
            return false;
        }
        root.appendChild(a);
        check(root.insertBefore(c, null) === c, 'null appends');
        check(root.insertBefore(b, c) === b && root.firstChild === a &&
              a.nextSibling === b && b.nextSibling === c, 'insert B in [A,C]');
        check(root.insertBefore(c, a) === c && root.insertBefore(a, a) === a,
              'move C before A and leave A in place');
        check(rejects(() => root.insertBefore(a, root)) && rejects(() => root.insertBefore(a, new Node())) &&
              rejects(() => root.removeChild(a.firstChild)) && rejects(() => root.removeChild({})) &&
              rejects(() => a.insertBefore(root, null)), 'invalid mutations throw');
        check(root.firstChild === c && c.nextSibling === a && a.nextSibling === b &&
              b.nextSibling === null, 'failed mutations preserve [C,A,B]');
        let bubbles = 0;
        root.addEventListener('child-event', () => { bubbles++; });
        a.firstChild.dispatchEvent(new Event('child-event', { bubbles: true }));
        check(root.removeChild(a) === a && a.parentNode === null && a.textContent === 'a' &&
              c.nextSibling === b, 'remove A without destroying its subtree');
        a.firstChild.dispatchEvent(new Event('child-event', { bubbles: true }));
        check(bubbles === 1, 'detached subtree does not bubble to the old parent');
        "#,
    )?;
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

#[test]
fn restores_dirty_checkboxes_and_rolls_back_canceled_activation() -> Result<(), String> {
    let build = compile_module(
        r#"
        const root = document.getElementById('root');
        const checkbox = document.createElement('input');
        checkbox.type = 'checkbox';
        checkbox.id = 'checkbox';
        checkbox.setAttribute('checked', 'false');
        if (!checkbox.defaultChecked || !checkbox.checked) throw new Error('clean checkedness follows presence');
        checkbox.removeAttribute('checked');
        if (checkbox.checked) throw new Error('clean checkedness follows removal');
        checkbox.defaultChecked = true;
        if (!checkbox.checked) throw new Error('clean checkedness follows defaultChecked');
        checkbox.checked = 0;
        root.appendChild(checkbox);
        const detached = document.createElement('input');
        detached.type = 'checkbox';
        detached.checked = {};
        let seen = 0;
        root.addEventListener('click', event => {
            if (event.target !== checkbox || !checkbox.checked) throw new Error('missing pre-activation');
            if (++seen === 1) event.preventDefault();
        });
        export function check() {
            return !checkbox.checked && checkbox.defaultChecked && detached.checked &&
                   !detached.defaultChecked && detached.parentNode === null &&
                   !Object.hasOwn(checkbox, 'checked');
        }
        export function verifyDirtyFlags() {
            checkbox.defaultChecked = false;
            checkbox.defaultChecked = true;
            detached.setAttribute('checked', '');
            detached.removeAttribute('checked');
            return check();
        }
        export function clicks() { return seen; }
        "#,
    )?;
    let mut session = BlitzSession::new(build.clone())?;
    assert_eq!(session.start("check", vec![])?, Value::boolean(true));
    let bytes = session.snapshot(&[])?.to_bytes()?;
    drop(session);
    let mut session = BlitzSession::restore(
        Snapshot::from_bytes(&bytes)?,
        build,
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )?;
    assert_eq!(
        session.start("verifyDirtyFlags", vec![])?,
        Value::boolean(true)
    );
    let target = session.document().get_element_by_id("checkbox").unwrap();
    session.resolve_layout(0.0);
    let native_checked = |session: &BlitzSession| {
        session
            .document()
            .get_node(target)
            .unwrap()
            .element_data()
            .unwrap()
            .checkbox_input_checked()
    };
    assert_eq!(native_checked(&session), Some(false));
    deliver_blitz_click(&mut session, target);
    assert_eq!(session.start("clicks", vec![])?, Value::number(1.0));
    assert_eq!(native_checked(&session), Some(false));
    assert_eq!(session.start("check", vec![])?, Value::boolean(true));
    deliver_blitz_click(&mut session, target);
    assert_eq!(session.start("clicks", vec![])?, Value::number(2.0));
    assert_eq!(native_checked(&session), Some(true));
    Ok(())
}

#[test]
fn resumes_pending_scheduler_wake_without_replaying_canceled_work() -> Result<(), String> {
    let build = compile_module(
        r#"
        const root = document.getElementById('root');
        root.textContent = 'waiting';
        clearTimeout(setTimeout(() => { throw new Error('canceled timer ran'); }, 0));
        setTimeout(() => { root.textContent += '!'; }, 0);
        "#,
    )?;
    let session = BlitzSession::new(build.clone())?;
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
    restored.run_until_idle()?;
    restored.run_until_idle()?;
    let doc = restored.document();
    let root = doc
        .get_node(doc.get_element_by_id("root").unwrap())
        .unwrap();
    assert_eq!(root.text_content(), "waiting!");
    Ok(())
}
