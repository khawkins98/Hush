// Stub for `@tauri-apps/api/event` used in Playwright e2e mode.
//
// Tests can simulate backend-emitted events by calling
// `window.__hush_e2e.fire(name, payload)` from the page (e.g. via
// `page.evaluate`). All currently-attached `listen` callbacks for
// that event name fire synchronously.
//
// Subscribers register with `listen(name, cb)` and receive an
// `unlisten` function — same shape as the real Tauri API. Tests do
// not normally need to manage unlisteners; component teardown does.

type Listener<T = unknown> = (event: { event: string; payload: T }) => void;
type UnlistenFn = () => void;

interface E2EBus {
  listeners: Map<string, Set<Listener>>;
  fire: <T>(name: string, payload: T) => void;
}

declare global {
  interface Window {
    __hush_e2e_event_bus?: E2EBus;
  }
}

function bus(): E2EBus {
  if (!window.__hush_e2e_event_bus) {
    const listeners = new Map<string, Set<Listener>>();
    window.__hush_e2e_event_bus = {
      listeners,
      fire<T>(name: string, payload: T) {
        const set = listeners.get(name);
        if (!set) return;
        for (const cb of set) {
          cb({ event: name, payload: payload as unknown });
        }
      },
    };
  }
  return window.__hush_e2e_event_bus;
}

export async function listen<T>(
  name: string,
  callback: (event: { event: string; payload: T }) => void,
): Promise<UnlistenFn> {
  const b = bus();
  let set = b.listeners.get(name);
  if (!set) {
    set = new Set();
    b.listeners.set(name, set);
  }
  const cb = callback as Listener;
  set.add(cb);
  return () => {
    set!.delete(cb);
  };
}

export type { UnlistenFn };
