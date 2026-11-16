import { useQueryClient } from '@tanstack/react-query';
import { useRouter } from '@tanstack/react-router';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useEffect } from 'react';

/**
 * The native shell (tauri/src-tauri/src/shell) draws the window's frame and
 * some screens itself, and shows this page only for the rest. It loads the
 * page with `?shell=native`; the flag is kept for the session because the
 * router drops the query on its first redirect.
 */
const FLAG = 'kass-native-shell';

function readFlag(): boolean {
  if (typeof window === 'undefined') return false;
  try {
    if (new URLSearchParams(window.location.search).get('shell') === 'native') {
      window.sessionStorage.setItem(FLAG, '1');
      return true;
    }
    return window.sessionStorage.getItem(FLAG) === '1';
  } catch {
    return false;
  }
}

export const nativeShell = readFlag();

/** Settings pages the shell draws itself (tauri/src-tauri/src/settings). */
const NATIVE_PAGES = [
  '/settings',
  '/settings/dictation',
  '/settings/command-mode',
  '/settings/transcription',
  '/settings/writing-style',
  '/settings/dictionary',
  '/settings/features',
  '/settings/logs',
];

export function isNativePage(path: string): boolean {
  const trimmed = path.length > 1 ? path.replace(/\/+$/, '') : path;
  return NATIVE_PAGES.includes(trimmed);
}

/**
 * Keep the shell and this page in step (bridge.rs): report where the router
 * is, follow the shell's navigation and refetch what it changed. The
 * preferences both use are the app's (lib/nativePrefs.ts). Does nothing
 * outside the shell.
 */
export function useNativeShell() {
  const router = useRouter();
  const queryClient = useQueryClient();

  useEffect(() => {
    if (!nativeShell) return;

    const report = () => {
      const { pathname, searchStr } = router.state.location;
      invoke('shell_location', { path: pathname, search: searchStr }).catch(() => {});
    };
    report();
    const offRouter = router.subscribe('onResolved', report);

    const offEvents = [
      listen<{ path: string; search: string }>('shell:navigate', ({ payload }) => {
        router.history.push(payload.path + payload.search);
      }),
      listen<{ keys: string[][] }>('shell:invalidate', ({ payload }) => {
        for (const queryKey of payload.keys) queryClient.invalidateQueries({ queryKey });
      }),
    ];

    return () => {
      offRouter();
      for (const off of offEvents) off.then((unlisten) => unlisten());
    };
  }, [router, queryClient]);
}
