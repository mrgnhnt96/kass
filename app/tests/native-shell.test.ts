import { expect, test } from 'bun:test';

const { isNativePage } = await import('../src/lib/nativeShell');

test('the shell draws General, Transcription, Dictionary, Features and Logs', () => {
  expect(isNativePage('/settings')).toBe(true);
  expect(isNativePage('/settings/')).toBe(true);
  expect(isNativePage('/settings/logs')).toBe(true);
  expect(isNativePage('/settings/dictionary')).toBe(true);
  expect(isNativePage('/settings/dictation')).toBe(false);
  expect(isNativePage('/captures')).toBe(false);
});
