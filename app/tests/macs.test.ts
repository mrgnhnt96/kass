import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { captureOrigin, macHeaders, useMacsStore, withMacParams } from '../src/lib/macs';

const studio = { id: 'studio-id', name: 'Morgan’s Studio' };

describe('captureOrigin', () => {
  test('a capture this Mac dictated is from this Mac', () => {
    expect(captureOrigin({ mac_id: 'studio-id', mac_name: 'Studio' }, studio)).toEqual({
      kind: 'this',
    });
  });

  test('a capture from another Mac carries its name', () => {
    expect(captureOrigin({ mac_id: 'laptop-id', mac_name: 'Laptop' }, studio)).toEqual({
      kind: 'other',
      name: 'Laptop',
    });
    expect(captureOrigin({ mac_id: 'laptop-id' }, studio)).toEqual({ kind: 'other', name: null });
  });

  test('unknown without a Mac on the capture or this Mac read', () => {
    expect(captureOrigin({ mac_id: null }, studio)).toBeNull();
    expect(captureOrigin({}, studio)).toBeNull();
    expect(captureOrigin({ mac_id: 'studio-id' }, null)).toBeNull();
  });
});

describe('saying which Mac sent a request', () => {
  beforeEach(() => useMacsStore.setState({ thisMac: null }));
  // Other tests build URLs and requests without a Mac.
  afterEach(() => useMacsStore.setState({ thisMac: null }));

  test('nothing until this Mac is read', () => {
    expect(macHeaders()).toEqual({});
    expect(withMacParams(new URL('ws://studio.local:17493/captures/stream')).search).toBe('');
  });

  test('headers percent-encode the name', () => {
    useMacsStore.setState({ thisMac: studio });
    expect(macHeaders()).toEqual({
      'X-Kass-Mac': 'studio-id',
      'X-Kass-Mac-Name': 'Morgan%E2%80%99s%20Studio',
    });
  });

  test('query parameters carry the same', () => {
    useMacsStore.setState({ thisMac: studio });
    const url = withMacParams(new URL('http://studio.local:17493/captures/events'));
    expect(url.searchParams.get('mac')).toBe('studio-id');
    expect(url.searchParams.get('mac_name')).toBe('Morgan’s Studio');
  });
});
