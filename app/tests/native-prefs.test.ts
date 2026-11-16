import { beforeEach, expect, mock, spyOn, test } from 'bun:test';

interface View {
  theme: 'system' | 'light' | 'dark';
  shareOnNetwork: boolean;
  remoteServerUrl: string | null;
  sharedAway: boolean;
}

// The app already holds its own values; the page's old ones are ignored.
const native: View = {
  theme: 'dark',
  shareOnNetwork: true,
  remoteServerUrl: 'http://studio.local:17493',
  sharedAway: false,
};
const invoke = mock(async (_command: string, _args?: unknown) => native as unknown);
let follow: ((event: { payload: View }) => void) | null = null;
mock.module('@tauri-apps/api/core', () => ({ invoke }));
mock.module('@tauri-apps/api/event', () => ({
  listen: async (_event: string, handler: (event: { payload: View }) => void) => {
    follow = handler;
    return () => {};
  },
}));

const { loadNativePrefs } = await import('../src/lib/nativePrefs');
const { queryClient } = await import('../src/lib/queryClient');
const { useServerStore } = await import('../src/stores/serverStore');
const { useUIStore } = await import('../src/stores/uiStore');
const refetch = spyOn(queryClient, 'invalidateQueries');

beforeEach(() => refetch.mockClear());

test('the page hands over what it saved and takes the app’s values', async () => {
  useUIStore.setState({ theme: 'light' });
  useServerStore.setState({ shareOnNetwork: false, remoteServerUrl: null, sharedAway: false });
  await loadNativePrefs();
  expect(invoke).toHaveBeenCalledWith('prefs_adopt', {
    prefs: { theme: 'light', shareOnNetwork: false, remoteServerUrl: null },
  });
  expect(useUIStore.getState().theme).toBe('dark');
  expect(useServerStore.getState().shareOnNetwork).toBe(true);
  expect(useServerStore.getState().remoteServerUrl).toBe('http://studio.local:17493');
  // Only once per launch.
  await loadNativePrefs();
  expect(invoke).toHaveBeenCalledTimes(1);
});

test('the page follows changes made in the app', () => {
  follow?.({ payload: { ...native, theme: 'system', remoteServerUrl: null } });
  expect(useUIStore.getState().theme).toBe('system');
  expect(useServerStore.getState().remoteServerUrl).toBeNull();
  expect(refetch).not.toHaveBeenCalled();
});

test('when the other Mac goes away everything is fetched again from this one', () => {
  follow?.({ payload: { ...native, sharedAway: true } });
  expect(useServerStore.getState().sharedAway).toBe(true);
  expect(refetch).toHaveBeenCalledTimes(1);
});
