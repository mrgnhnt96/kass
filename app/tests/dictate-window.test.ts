import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createElement } from 'react';
import { act, create } from 'react-test-renderer';

type Handler = (event: { payload: unknown }) => void;
const handlers = new Map<string, Handler>();
const invoke = mock(async (_command: string, _args?: unknown) => undefined as unknown);
const platform = { metadata: { isTauri: true } };
const settings = { input_device_id: null as string | null };
const serverState = { customModelsDir: null };

mock.module('react-i18next', () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
mock.module('@tauri-apps/api/event', () => ({
  listen: async (name: string, handler: Handler) => {
    handlers.set(name, handler);
    return () => handlers.delete(name);
  },
  emit: async () => {},
}));
mock.module('@tauri-apps/api/core', () => ({ invoke }));
mock.module('../src/platform/PlatformContext', () => ({
  usePlatform: () => platform,
  PlatformProvider: ({ children }: { children: unknown }) => children,
}));
mock.module('../src/lib/hooks/useSettings', () => ({ useCaptureSettings: () => ({ settings }) }));
mock.module('../src/stores/serverStore', () => ({
  serverUrl: () => 'http://127.0.0.1:17493',
  useServerUrl: () => 'http://127.0.0.1:17493',
  remoteServerUrl: () => null,
  sharesOnNetwork: () => false,
  useServerStore: Object.assign(
    (select: (state: typeof serverState) => unknown) => select(serverState),
    { getState: () => serverState },
  ),
}));

const { DictateWindow } = await import('../src/components/DictateWindow/DictateWindow');

let renderer: ReturnType<typeof create> | undefined;

async function mount() {
  const client = new QueryClient();
  await act(async () => {
    renderer = create(createElement(QueryClientProvider, { client }, createElement(DictateWindow)));
  });
}

beforeEach(() => {
  handlers.clear();
  invoke.mockClear();
  const style = { background: '' };
  Object.assign(globalThis, {
    window: Object.assign(globalThis, { location: { origin: 'tauri://localhost' } }),
    document: { documentElement: { style }, body: { style: { background: '' } } },
    IS_REACT_ACT_ENVIRONMENT: true,
  });
});

afterEach(async () => {
  if (renderer) await act(async () => renderer?.unmount());
  renderer = undefined;
});

test('the desktop pill follows native dictation and never uses browser audio', async () => {
  platform.metadata.isTauri = true;
  const getUserMedia = mock(async () => ({}));
  Object.defineProperty(globalThis, 'navigator', {
    configurable: true,
    value: { mediaDevices: { getUserMedia } },
  });
  await mount();
  expect(handlers.has('dictate:start')).toBe(false);
  expect(handlers.has('dictation:state')).toBe(true);
  expect(invoke.mock.calls.map((call) => call[0])).toContain('dictation_configure');
  // Idle, the pill's slot is empty.
  expect(renderer?.toJSON()).toMatchObject({ children: [{ children: null }] });
  await act(async () =>
    handlers.get('dictation:state')?.({ payload: { take: 1, state: 'recording' } }),
  );
  await act(async () =>
    handlers.get('dictation:state')?.({ payload: { take: 1, state: 'preparing' } }),
  );
  expect(renderer?.toJSON()).toMatchObject({ children: [{ children: [expect.anything()] }] });
  expect(getUserMedia).not.toHaveBeenCalled();
});

test('the web build keeps the browser chord path', async () => {
  platform.metadata.isTauri = false;
  await mount();
  expect(handlers.has('dictate:start')).toBe(true);
  expect(handlers.has('dictation:state')).toBe(false);
});
