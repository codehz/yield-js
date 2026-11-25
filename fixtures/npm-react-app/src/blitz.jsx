import { flushSync } from 'react-dom';
import { createRoot } from 'react-dom/client';

function App() {
  return <main><h1>Hello from React DOM</h1></main>;
}

const root = createRoot(document.getElementById('root'));
flushSync(() => root.render(<App />));
