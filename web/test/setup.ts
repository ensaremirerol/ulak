import { afterAll, afterEach } from 'vitest';
import { cleanup } from '@testing-library/preact';

afterEach(() => cleanup());

// Preact's hooks schedule a 100 ms fallback timer after paint that calls cancelAnimationFrame;
// let it fire while jsdom (and that global) still exists, not after the file's teardown.
afterAll(() => new Promise<void>((done) => setTimeout(done, 150)));
