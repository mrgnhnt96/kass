import { expect, test } from 'bun:test';

const { isNativePage } = await import('../src/lib/nativeShell');

test('the shell draws General, Transcription, Features and Logs', () => {
  expect(isNativePage('/settings')).toBe(true);
  expect(isNativePage('/settings/')).toBe(true);
  expect(isNativePage('/settings/logs')).toBe(true);
  expect(isNativePage('/settings/dictionary')).toBe(false);
  expect(isNativePage('/captures')).toBe(false);
});
