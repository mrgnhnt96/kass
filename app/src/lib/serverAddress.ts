/** The bundled server on this Mac. */
export const LOCAL_SERVER_URL = 'http://127.0.0.1:17493';

const SERVER_PORT = 17493;

/**
 * Turn what someone types for another Mac (`studio.local`,
 * `192.168.1.20:17493`, `http://studio.local/`) into a server URL, or null
 * when it isn't one. An http address without a port gets Kass's own.
 */
export function normalizeServerAddress(input: string): string | null {
  const text = input.trim();
  if (!text) return null;
  const hasScheme = /^[a-z][a-z\d+.-]*:\/\//i.test(text);
  let url: URL;
  try {
    url = new URL(hasScheme ? text : `http://${text}`);
  } catch {
    return null;
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return null;
  if (!url.hostname || url.username || url.password) return null;
  if (url.pathname !== '/' || url.search || url.hash) return null;
  // URL drops a port that's the scheme's default, so look at what was typed.
  // An https address is something in front of Kass, on its own port.
  const typedPort = /:\d+\/?$/.test(text);
  const port = url.port || (typedPort || url.protocol === 'https:' ? '' : String(SERVER_PORT));
  return `${url.protocol}//${url.hostname}${port ? `:${port}` : ''}`;
}
