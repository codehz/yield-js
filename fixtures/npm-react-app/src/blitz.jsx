import { useState } from 'react';
import { flushSync } from 'react-dom';
import { createRoot } from 'react-dom/client';

function App() {
  const [count, setCount] = useState(0);
  return <main><h1>Hello from React DOM</h1><button id="counter" onClick={() => setCount(n => n + 1)}>Clicks: {count}</button></main>;
}

const root = createRoot(document.getElementById('root'));
flushSync(() => root.render(<App />));
