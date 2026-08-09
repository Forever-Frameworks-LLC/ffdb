import "@testing-library/jest-dom/vitest";

// Node 24 exposes an incomplete process-level localStorage unless it receives
// --localstorage-file. Keep the configured test command self-contained with a
// standards-shaped in-memory browser store for both storage namespaces.
function createTestStorage(): Storage {
  const values = new Map<string, string>();
  return {
    get length() { return values.size; },
    clear: () => values.clear(),
    getItem: (key) => values.get(key) ?? null,
    key: (index) => [...values.keys()][index] ?? null,
    removeItem: (key) => { values.delete(key); },
    setItem: (key, value) => { values.set(String(key), String(value)); },
  };
}
Object.defineProperty(globalThis, "localStorage", { configurable: true, value: createTestStorage() });
Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: createTestStorage() });

// CodeMirror measures text ranges in a browser. jsdom does not implement
// these geometry methods, so provide stable empty rectangles for component
// tests while leaving real-browser measurements untouched.
if (Range.prototype.getClientRects === undefined) {
  Object.defineProperty(Range.prototype, "getClientRects", {
    value: () => ({ length: 0, item: () => null, [Symbol.iterator]: function* iterator() { return; } }),
  });
}
if (Range.prototype.getBoundingClientRect === undefined) {
  Object.defineProperty(Range.prototype, "getBoundingClientRect", {
    value: () => ({ bottom: 0, height: 0, left: 0, right: 0, top: 0, width: 0, x: 0, y: 0, toJSON: () => ({}) }),
  });
}

// The v1-derived resizable SQL workspace observes its panel group. jsdom has
// no layout engine or ResizeObserver, so report one stable measurement.
if (globalThis.ResizeObserver === undefined) {
  class TestResizeObserver implements ResizeObserver {
    constructor(_callback: ResizeObserverCallback) {}
    observe(_target: Element) {}
    unobserve() {}
    disconnect() {}
  }
  Object.defineProperty(globalThis, "ResizeObserver", { configurable: true, value: TestResizeObserver });
}
