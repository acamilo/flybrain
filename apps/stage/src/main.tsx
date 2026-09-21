import { createRoot } from 'react-dom/client';

import { App } from './App';
import './index.css';

const root = document.getElementById('root');
if (!root) throw new Error('#root is missing from index.html');

/**
 * Deliberately no `StrictMode`.
 *
 * StrictMode double-invokes effects in development, and this page's single effect builds a paint
 * loop, a worker, an AudioContext and a feed connection. They are all disposed correctly, but
 * running two of each while measuring paint timings and recording mockups makes every number a
 * lie. Correctness here is checked by the e2e suite against the real build, not by a dev-only
 * double render.
 */
createRoot(root).render(<App />);
