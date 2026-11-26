import './blitz-styles.mjs';
import './styles.css';
import { useState } from 'react';
import { flushSync } from 'react-dom';
import { createRoot } from 'react-dom/client';

function App() {
  const [count, setCount] = useState(0);
  const [name, setName] = useState('');
  return (
    <main>
      <h1>Hello from React DOM</h1>
      <button id="counter" onClick={() => setCount(n => n + 1)}>Clicks: {count}</button>
      <div>
        <label htmlFor="name">Name</label>
        <input id="name" type="text" name="name" value={name} onChange={event => setName(event.target.value)} />
        <button id="clear-name" onClick={() => setName('')}>Clear</button>
        <p id="greeting">Hello, {name || 'guest'}!</p>
      </div>
    </main>
  );
}

const root = createRoot(document.getElementById('root'));
flushSync(() => root.render(<App />));
