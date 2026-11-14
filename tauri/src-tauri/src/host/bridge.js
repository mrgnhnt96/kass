// Stands in for Tauri's page API, so @tauri-apps/api works unchanged.
// Calls go to Rust through wry's `window.ipc`; Rust answers through
// `__TAURI_INTERNALS__.__resolve` and sends events through `__KASS_EMIT__`.
(function () {
  const LABEL = __LABEL__;
  const callbacks = new Map();
  const pending = new Map();
  const listeners = new Map();
  let next = 1;

  const transformCallback = (callback, once = false) => {
    const id = next++;
    callbacks.set(id, (data) => {
      if (once) callbacks.delete(id);
      return callback && callback(data);
    });
    return id;
  };

  const base64 = (bytes) => {
    let text = '';
    for (let i = 0; i < bytes.length; i += 0x8000) {
      text += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    }
    return btoa(text);
  };

  const encode = (args) => {
    if (args instanceof ArrayBuffer) return { __bytes: base64(new Uint8Array(args)) };
    if (ArrayBuffer.isView(args)) {
      return { __bytes: base64(new Uint8Array(args.buffer, args.byteOffset, args.byteLength)) };
    }
    return args === undefined ? {} : args;
  };

  const headersOf = (options) => {
    const headers = options && options.headers;
    if (!headers) return {};
    if (typeof Headers !== 'undefined' && headers instanceof Headers) {
      return Object.fromEntries(headers.entries());
    }
    return headers;
  };

  const send = (cmd, args, options) =>
    new Promise((resolve, reject) => {
      const id = next++;
      pending.set(id, { resolve, reject });
      try {
        window.ipc.postMessage(
          JSON.stringify({ id, cmd, args: encode(args), headers: headersOf(options) }),
        );
      } catch (error) {
        pending.delete(id);
        reject(String(error));
      }
    });

  window.isTauri = true;
  window.__TAURI_INTERNALS__ = {
    metadata: {
      currentWindow: { label: LABEL },
      currentWebview: { windowLabel: LABEL, label: LABEL },
    },
    transformCallback,
    unregisterCallback: (id) => callbacks.delete(id),
    runCallback: (id, data) => {
      const callback = callbacks.get(id);
      if (callback) callback(data);
    },
    callbacks,
    convertFileSrc: (path, protocol = 'asset') =>
      `${protocol}://localhost/${encodeURIComponent(path)}`,
    invoke(cmd, args = {}, options) {
      if (cmd === 'plugin:event|listen') {
        const id = next++;
        if (!listeners.has(args.event)) listeners.set(args.event, new Map());
        listeners
          .get(args.event)
          .set(id, { handler: args.handler, target: args.target || { kind: 'Any' } });
        return Promise.resolve(id);
      }
      if (cmd === 'plugin:event|unlisten') {
        const list = listeners.get(args.event);
        if (list) list.delete(args.eventId);
        return Promise.resolve();
      }
      return send(cmd, args, options);
    },
    __resolve(id, ok, value) {
      const call = pending.get(id);
      if (!call) return;
      pending.delete(id);
      if (ok) call.resolve(value);
      else call.reject(value);
    },
  };

  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener(event, id) {
      const list = listeners.get(event);
      if (list) list.delete(id);
    },
  };

  // A listener for any window hears every event; one for a single window
  // hears events sent to everyone or to that window.
  window.__KASS_EMIT__ = (event, payload, target) => {
    const list = listeners.get(event);
    if (!list) return;
    for (const [id, listener] of [...list]) {
      const scope = listener.target;
      if (scope.kind !== 'Any' && target != null && scope.label !== target) continue;
      const callback = callbacks.get(listener.handler);
      if (!callback) continue;
      try {
        callback({ event, id, payload });
      } catch (error) {
        console.error(error);
      }
    }
  };

  // `data-tauri-drag-region` moves the window; a double click zooms it.
  document.addEventListener('mousedown', (event) => {
    const region = event.target;
    if (event.button !== 0 || !(region instanceof Element)) return;
    if (!region.hasAttribute('data-tauri-drag-region')) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    send(event.detail === 2 ? 'plugin:window|internal_toggle_maximize' : 'plugin:window|start_dragging', {});
  });
})();
