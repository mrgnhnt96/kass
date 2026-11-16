import { beforeEach, expect, mock, test } from 'bun:test';

const invoke = mock(async (_command: string, _args?: unknown) => 'beta' as unknown);
const openOnboarding = mock(() => {});
mock.module('@tauri-apps/api/core', () => ({ invoke }));
mock.module('@tauri-apps/api/event', () => ({ listen: async () => () => {} }));
mock.module('../src/components/Onboarding/openOnboarding', () => ({ openOnboarding }));

const { isNativePage, runCommand, synced } = await import('../src/lib/nativeShell');
const { useServerStore } = await import('../src/stores/serverStore');
const { useUpdateChannelStore } = await import('../src/lib/betaFeatures');

function platform({ fail = false } = {}) {
  const restartApp = mock(async () => {
    if (fail) throw new Error('restart failed');
  });
  const restartServer = mock(async () => {
    if (fail) throw new Error('restart failed');
    return 'http://127.0.0.1:17493';
  });
  return {
    restartApp,
    restartServer,
    value: { lifecycle: { restartApp, restartServer } } as never,
  };
}

beforeEach(() => {
  invoke.mockClear();
  openOnboarding.mockClear();
  useServerStore.setState({ shareOnNetwork: false, remoteServerUrl: null, sharedAway: false });
  useUpdateChannelStore.setState({ channel: 'beta' });
});

test('the shell draws General, Transcription, Dictionary, Features and Logs', () => {
  expect(isNativePage('/settings')).toBe(true);
  expect(isNativePage('/settings/')).toBe(true);
  expect(isNativePage('/settings/logs')).toBe(true);
  expect(isNativePage('/settings/dictionary')).toBe(true);
  expect(isNativePage('/settings/dictation')).toBe(false);
  expect(isNativePage('/captures')).toBe(false);
});

test('sharing restarts the server, and rolls back when that fails', async () => {
  const ok = platform();
  await runCommand({ kind: 'setShareOnNetwork', share: true }, ok.value);
  expect(ok.restartServer).toHaveBeenCalledTimes(1);
  expect(useServerStore.getState().shareOnNetwork).toBe(true);

  const broken = platform({ fail: true });
  await expect(
    runCommand({ kind: 'setShareOnNetwork', share: false }, broken.value),
  ).rejects.toThrow('restart failed');
  expect(useServerStore.getState().shareOnNetwork).toBe(true);
});

test('using another Mac restarts onto it, and keeps the old one when that fails', async () => {
  const broken = platform({ fail: true });
  await expect(
    runCommand({ kind: 'useRemote', url: 'http://studio.local:17493' }, broken.value),
  ).rejects.toThrow('restart failed');
  expect(useServerStore.getState().remoteServerUrl).toBeNull();

  const ok = platform();
  await runCommand({ kind: 'useRemote', url: 'http://studio.local:17493' }, ok.value);
  expect(ok.restartApp).toHaveBeenCalledTimes(1);
  expect(synced().remoteServerUrl).toBe('http://studio.local:17493');
  expect(synced().serverUrl).toBe('http://studio.local:17493');
});

test('leaving the beta while on another Mac restarts back onto this one', async () => {
  useServerStore.setState({ remoteServerUrl: 'http://studio.local:17493' });
  invoke.mockImplementationOnce(async () => 'stable');
  const ok = platform();
  await runCommand({ kind: 'setBeta', beta: false }, ok.value);
  expect(invoke).toHaveBeenCalledWith('set_update_channel', { channel: 'stable' });
  expect(useUpdateChannelStore.getState().channel).toBe('stable');
  expect(ok.restartApp).toHaveBeenCalledTimes(1);
  expect(synced().remoteServerUrl).toBeNull();
});

test('onboarding opens from the first step', async () => {
  await runCommand({ kind: 'openOnboarding' }, platform().value);
  expect(openOnboarding).toHaveBeenCalledTimes(1);
});
