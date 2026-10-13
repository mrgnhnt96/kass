import { invoke } from '@tauri-apps/api/core';
import { create } from 'zustand';

/**
 * Which Mac this is and which others are connected, for a Kass shared over
 * the network (beta `network`). Requests that can save a capture say which
 * Mac sent them (backend/utils/macs.py), and the Captures inspector shows
 * where each came from while another Mac is connected.
 */
export interface Mac {
  id: string;
  name: string | null;
}

interface MacsStore {
  /** This Mac, once read from the app (`this_mac.rs`). */
  thisMac: Mac | null;
  /** Macs with an app listening to the shared server, this one included. */
  connected: Mac[];
  setConnected: (connected: Mac[]) => void;
}

export const useMacsStore = create<MacsStore>()((set) => ({
  thisMac: null,
  connected: [],
  setConnected: (connected) => set({ connected }),
}));

let thisMacRead: Promise<Mac | null> | null = null;

/** Read this Mac from the app, once per window. */
export function readThisMac(): Promise<Mac | null> {
  thisMacRead ??= invoke<Mac | null>('this_mac')
    .catch(() => null)
    .then((mac) => {
      useMacsStore.setState({ thisMac: mac });
      return mac;
    });
  return thisMacRead;
}

/** Headers that say which Mac sent a request. */
export function macHeaders(): Record<string, string> {
  const mac = useMacsStore.getState().thisMac;
  if (!mac) return {};
  const headers: Record<string, string> = { 'X-Kass-Mac': mac.id };
  if (mac.name) headers['X-Kass-Mac-Name'] = encodeURIComponent(mac.name);
  return headers;
}

/** The same as query parameters, for WebSocket and EventSource, which can't set headers. */
export function withMacParams(url: URL): URL {
  const mac = useMacsStore.getState().thisMac;
  if (mac) {
    url.searchParams.set('mac', mac.id);
    if (mac.name) url.searchParams.set('mac_name', mac.name);
  }
  return url;
}

/** Whether another Mac is connected to the shared server. */
export function useOtherMacConnected(): boolean {
  return useMacsStore((state) => {
    if (!state.thisMac) return false;
    const id = state.thisMac.id;
    return state.connected.some((mac) => mac.id !== id);
  });
}

/**
 * Where a capture was dictated: `this` Mac, another one by `name` (null when
 * it sent none), or null when it isn't known.
 */
export function captureOrigin(
  capture: { mac_id?: string | null; mac_name?: string | null },
  thisMac: Mac | null,
): { kind: 'this' } | { kind: 'other'; name: string | null } | null {
  if (!capture.mac_id || !thisMac) return null;
  if (capture.mac_id === thisMac.id) return { kind: 'this' };
  return { kind: 'other', name: capture.mac_name ?? null };
}
