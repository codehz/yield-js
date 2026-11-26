import './blitz-styles.mjs';
import './styles.css';
import { useState } from 'react';
import { flushSync } from 'react-dom';
import { createRoot } from 'react-dom/client';
import styles from './todo.module.css';

function App() {
  const [nextId, setNextId] = useState(1);
  const [todos, setTodos] = useState([]);
  const [filter, setFilter] = useState('all');
  const [draft, setDraft] = useState('');
  const [edit, setEdit] = useState(null);
  const active = todos.filter(todo => !todo.completed).length;
  const allChecked = todos.length > 0 && active === 0;
  const visible = todos.filter(todo => filter === 'all' || todo.completed === (filter === 'completed'));

  function add() {
    const title = draft.trim();
    if (!title) return;
    setTodos(items => [...items, { id: nextId, title, completed: false }]);
    setNextId(id => id + 1);
    setDraft('');
  }

  function remove(id) {
    setTodos(items => items.filter(todo => todo.id !== id));
    if (edit?.id === id) setEdit(null);
  }

  function save() {
    const title = edit.title.trim();
    if (title) {
      setTodos(items => items.map(todo => todo.id === edit.id ? { ...todo, title } : todo));
    } else {
      remove(edit.id);
    }
    setEdit(null);
  }

  return (
    <main className={styles.app}>
      <h1 className={styles.heading}>TodoMVC</h1>
      <div className={styles.controls}>
        <label htmlFor="new-todo">New task</label>
        <input id="new-todo" type="text" value={draft} onChange={event => setDraft(event.target.value)} />
        <button id="add-todo" onClick={add}>Add</button>
      </div>
      <div className={styles.controls}>
        <input id="toggle-all" type="checkbox" checked={allChecked} disabled={todos.length === 0}
          onChange={event => {
            const completed = event.target.checked;
            setTodos(items => items.map(todo => ({ ...todo, completed })));
          }} />
        <label htmlFor="toggle-all">Toggle all</label>
      </div>
      <div className={styles.controls} role="group" aria-label="Filter tasks">
        {['All', 'Active', 'Completed'].map(label => {
          const key = label.toLowerCase();
          return <button key={key} id={'filter-' + key} aria-pressed={filter === key}
            onClick={() => setFilter(key)}>{label}</button>;
        })}
      </div>
      <ul id="todo-list" className={styles.list}>
        {visible.map(todo => (
          <li id={'todo-' + todo.id} key={todo.id} className={todo.completed ? styles.completed : undefined}>
            <div className={styles.controls}>
              <input id={'toggle-' + todo.id} type="checkbox" checked={todo.completed}
                onChange={event => {
                  const completed = event.target.checked;
                  setTodos(items => items.map(item => item.id === todo.id ? { ...item, completed } : item));
                }} />
              <label className={styles.title} htmlFor={'toggle-' + todo.id}>{todo.title}</label>
              <button id={'edit-' + todo.id} onClick={() => setEdit({ id: todo.id, title: todo.title })}
                aria-label={'Edit ' + todo.title}>Edit</button>
              <button id={'delete-' + todo.id} onClick={() => remove(todo.id)}
                aria-label={'Delete ' + todo.title}>Delete</button>
            </div>
            {edit?.id === todo.id && (
              <div className={styles.controls}>
                <label htmlFor={'draft-' + todo.id}>Edit title</label>
                <input id={'draft-' + todo.id} type="text" value={edit.title}
                  onChange={event => setEdit({ id: todo.id, title: event.target.value })} />
                <button id={'save-' + todo.id} onClick={save}>Save</button>
                <button id={'cancel-' + todo.id} onClick={() => setEdit(null)}>Cancel</button>
              </div>
            )}
          </li>
        ))}
      </ul>
      <div className={styles.controls + ' ' + styles.footer}>
        <span id="todo-count" aria-live="polite">{active} left</span>
        <button id="clear-completed" disabled={active === todos.length} onClick={() => {
          setTodos(items => items.filter(todo => !todo.completed));
          if (edit && todos.some(todo => todo.id === edit.id && todo.completed)) setEdit(null);
        }}>Clear completed</button>
      </div>
    </main>
  );
}

const root = createRoot(document.getElementById('root'));
flushSync(() => root.render(<App />));
