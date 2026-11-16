import { useQueryClient } from '@tanstack/react-query';
import { useRouter } from '@tanstack/react-router';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useEffect } from 'react';
import { openOnboarding } from '@/components/Onboarding/openOnboarding';
import { type UpdateChannel, useUpdateChannelStore } from '@/lib/betaFeatures';
import { usePlatform } from '@/platform/PlatformContext';
import type { Platform } from '@/platform/types';
import { remoteServerUrl, serverUrl, sharesOnNetwork, useServerStore } from '@/stores/serverStore';
import { type Theme, useUIStore } from '@/stores/uiStore';

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
  '/settings/transcription',
  '/settings/writing-style',
  '/settings/features',
  '/settings/logs',
];

export function isNativePage(path: string): boolean {
  const trimmed = path.length > 1 ? path.replace(/\/+$/, '') : path;
  return NATIVE_PAGES.includes(trimmed);
}

/** The preferences this page still owns, as the shell reads them (bridge.rs `Synced`). */
export function synced() {
  return {
    theme: useUIStore.getState().theme,
    shareOnNetwork: useServerStore.getState().shareOnNetwork,
    remoteServerUrl: remoteServerUrl(),
    serverUrl: serverUrl(),
    sharedAway: useServerStore.getState().sharedAway,
  };
}

export type ShellCommand =
  | { kind: 'setTheme'; theme: Theme }
  | { kind: 'setBeta'; beta: boolean }
  | { kind: 'setShareOnNetwork'; share: boolean }
  | { kind: 'useRemote'; url: string | null }
  | { kind: 'openOnboarding' };

/**
 * Run a command the shell sent, the way the page's own settings did it
 * (BetaUpdatesRow, NetworkSection, FeaturesPage). Throws what failed.
 */
export async function runCommand(command: ShellCommand, platform: Platform): Promise<void> {
  switch (command.kind) {
    case 'setTheme':
      useUIStore.getState().setTheme(command.theme);
      return;
    case 'setBeta': {
      // Beta `network` stops with the beta: back onto this Mac's own server.
      const wasRemote = remoteServerUrl() !== null;
      const wasSharing = sharesOnNetwork();
      const channel = await invoke<UpdateChannel>('set_update_channel', {
        channel: command.beta ? 'beta' : 'stable',
      });
      useUpdateChannelStore.getState().setChannel(channel);
      if (!command.beta && wasRemote) await platform.lifecycle.restartApp();
      else if (!command.beta && wasSharing) await platform.lifecycle.restartServer();
      return;
    }
    case 'setShareOnNetwork': {
      const { setShareOnNetwork } = useServerStore.getState();
      setShareOnNetwork(command.share);
      try {
        await platform.lifecycle.restartServer();
      } catch (error) {
        setShareOnNetwork(!command.share);
        throw error;
      }
      return;
    }
    case 'useRemote': {
      const { remoteServerUrl: previous, setRemoteServerUrl } = useServerStore.getState();
      setRemoteServerUrl(command.url);
      try {
        await platform.lifecycle.restartApp();
      } catch (error) {
        setRemoteServerUrl(previous);
        throw error;
      }
      return;
    }
    case 'openOnboarding':
      openOnboarding();
      return;
  }
}

/**
 * Keep the shell and this page in step (bridge.rs): report where the router
 * is and the preferences above, follow the shell's navigation, run its
 * commands and refetch what it changed. Does nothing outside the shell.
 */
export function useNativeShell() {
  const router = useRouter();
  const queryClient = useQueryClient();
  const platform = usePlatform();

  useEffect(() => {
    if (!nativeShell) return;

    const report = () => {
      const { pathname, searchStr } = router.state.location;
      invoke('shell_location', { path: pathname, search: searchStr }).catch(() => {});
    };
    report();
    const offRouter = router.subscribe('onResolved', report);

    let last = '';
    const sync = () => {
      const next = synced();
      const key = JSON.stringify(next);
      if (key === last) return;
      last = key;
      invoke('shell_sync', { synced: next }).catch(() => {});
    };
    sync();
    const offStores = [
      useUIStore.subscribe(sync),
      useServerStore.subscribe(sync),
      useUpdateChannelStore.subscribe(sync),
    ];

    const offEvents = [
      listen<{ path: string; search: string }>('shell:navigate', ({ payload }) => {
        router.history.push(payload.path + payload.search);
      }),
      listen<ShellCommand & { id: number }>('shell:command', async ({ payload }) => {
        let error: string | null = null;
        try {
          await runCommand(payload, platform);
        } catch (e) {
          error = String(e);
        }
        invoke('shell_command_result', { id: payload.id, error }).catch(() => {});
      }),
      listen<{ keys: string[][] }>('shell:invalidate', ({ payload }) => {
        for (const queryKey of payload.keys) queryClient.invalidateQueries({ queryKey });
      }),
    ];

    return () => {
      offRouter();
      for (const off of offStores) off();
      for (const off of offEvents) off.then((unlisten) => unlisten());
    };
  }, [router, queryClient, platform]);
}
