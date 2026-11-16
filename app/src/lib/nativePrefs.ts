import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { queryClient } from '@/lib/queryClient';
import { useServerStore } from '@/stores/serverStore';
import { type Theme, useUIStore } from '@/stores/uiStore';

/** The app's own preferences (tauri/src-tauri/src/prefs.rs `page_view`). */
interface NativePrefs {
  theme: Theme;
  shareOnNetwork: boolean;
  remoteServerUrl: string | null;
  customModelsDir: string | null;
  /** The other Mac in use can't be reached; this Mac's server stands in. */
  sharedAway: boolean;
}

let loading: Promise<void> | null = null;

/**
 * The app keeps the theme and the beta `network` settings itself. The first
 * time, it takes what this page saved in localStorage; after that the page
 * only follows it (`prefs:changed`). Resolves once the stores hold the
 * app's values, before the server is started with them.
 */
export function loadNativePrefs(): Promise<void> {
  loading ??= (async () => {
    await listen<NativePrefs>('prefs:changed', ({ payload }) => apply(payload));
    const { theme } = useUIStore.getState();
    const { shareOnNetwork, remoteServerUrl, customModelsDir } = useServerStore.getState();
    apply(
      await invoke<NativePrefs>('prefs_adopt', {
        prefs: { theme, shareOnNetwork, remoteServerUrl, customModelsDir },
      }),
    );
  })().catch((error) => console.warn('[prefs] loading the app’s preferences failed:', error));
  return loading;
}

function apply(prefs: NativePrefs): void {
  const ui = useUIStore.getState();
  if (ui.theme !== prefs.theme) ui.setTheme(prefs.theme);
  const server = useServerStore.getState();
  if (server.shareOnNetwork !== prefs.shareOnNetwork) {
    server.setShareOnNetwork(prefs.shareOnNetwork);
  }
  if (server.remoteServerUrl !== prefs.remoteServerUrl) {
    server.setRemoteServerUrl(prefs.remoteServerUrl);
  }
  if (server.customModelsDir !== prefs.customModelsDir) {
    server.setCustomModelsDir(prefs.customModelsDir);
  }
  if (server.sharedAway !== prefs.sharedAway) {
    server.setSharedAway(prefs.sharedAway);
    // Everything shown came from the other server.
    void queryClient.invalidateQueries();
  }
}
