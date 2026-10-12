import { describe, expect, test } from 'bun:test';
import { normalizeServerAddress } from '../src/lib/serverAddress';

describe('normalizeServerAddress', () => {
  test("adds Kass's port and http to a bare Mac name", () => {
    expect(normalizeServerAddress('studio.local')).toBe('http://studio.local:17493');
    expect(normalizeServerAddress('  Studio.local  ')).toBe('http://studio.local:17493');
  });

  test('keeps a port that was typed', () => {
    expect(normalizeServerAddress('192.168.1.20:9000')).toBe('http://192.168.1.20:9000');
    expect(normalizeServerAddress('http://studio.local:80')).toBe('http://studio.local');
  });

  test('accepts a full URL with a trailing slash', () => {
    expect(normalizeServerAddress('http://studio.local:17493/')).toBe('http://studio.local:17493');
    expect(normalizeServerAddress('https://kass.example')).toBe('https://kass.example');
  });

  test('rejects what is not a server address', () => {
    expect(normalizeServerAddress('')).toBeNull();
    expect(normalizeServerAddress('   ')).toBeNull();
    expect(normalizeServerAddress('ftp://studio.local')).toBeNull();
    expect(normalizeServerAddress('studio.local/health')).toBeNull();
    expect(normalizeServerAddress('http://user:pw@studio.local')).toBeNull();
    expect(normalizeServerAddress('not a host')).toBeNull();
  });
});
