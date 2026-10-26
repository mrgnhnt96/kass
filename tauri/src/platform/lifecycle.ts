import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { PlatformLifecycle, ServerLogEntry } from '@/platform/types';
import { remoteServerUrl, sharesOnNetwork } from '@/stores/serverStore';

class TauriLifecycle implements PlatformLifecycle {
  onServerReady?: () => void;

  async startServer(modelsDir?: string | null): Promise<string> {
    try {
      const result = await invoke<string>('start_server', {
        modelsDir: modelsDir ?? undefined,
        shareOnNetwork: sharesOnNetwork(),
        // Beta `network`: this Mac's server stands in for the other Mac's.
        sharedServer: remoteServerUrl() ?? '',
      });
      console.log('Server started:', result);
      this.onServerReady?.();
      return result;
    } catch (error) {
      console.error('Failed to start server:', error);
      throw error;
    }
  }

  async restartApp(): Promise<void> {
    await invoke('restart_app');
  }

  async stopServer(): Promise<void> {
    try {
      await invoke('stop_server');
      console.log('Server stopped');
    } catch (error) {
      console.error('Failed to stop server:', error);
      throw error;
    }
  }

  async restartServer(modelsDir?: string | null): Promise<string> {
    try {
      const result = await invoke<string>('restart_server', {
        modelsDir: modelsDir ?? undefined,
        shareOnNetwork: sharesOnNetwork(),
        // Beta `network`: this Mac's server stands in for the other Mac's.
        sharedServer: remoteServerUrl() ?? '',
      });
      console.log('Server restarted:', result);
      this.onServerReady?.();
      return result;
    } catch (error) {
      console.error('Failed to restart server:', error);
      throw error;
    }
  }

  subscribeToServerLogs(callback: (entry: ServerLogEntry) => void): () => void {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    void listen<ServerLogEntry>('server-log', (event) => {
      callback(event.payload);
    })
      .then((fn) => {
        if (disposed) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch((error) => {
        console.error('Failed to subscribe to server logs:', error);
      });

    return () => {
      disposed = true;
      unlisten?.();
      unlisten = null;
    };
  }
}

export const tauriLifecycle = new TauriLifecycle();
