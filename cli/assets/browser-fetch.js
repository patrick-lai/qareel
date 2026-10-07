async (request) => {
  const started = performance.now();
  const target = new URL(request.url, location.href);
  if (target.origin !== location.origin) throw new Error(`browser.fetch_cross_origin: ${target.origin} is not this tab's origin ${location.origin}; navigate to that site first or pass a path such as /rest/api/2/issue/KEY`);
  const headers = new Headers();
  for (const [name, value] of Object.entries(request.headers || {})) headers.set(name, String(value));
  if (!headers.has('accept')) headers.set('accept', 'application/json, text/plain;q=0.8, */*;q=0.5');
  let body;
  if (request.body !== undefined && request.body !== null) {
    body = typeof request.body === 'string' ? request.body : JSON.stringify(request.body);
    if (!headers.has('content-type')) headers.set('content-type', 'application/json');
  }
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), 20000);
  try {
    const response = await fetch(target.href, { method: request.method, headers, body, credentials: 'same-origin', redirect: 'follow', cache: 'no-store', signal: abort.signal });
    if (response.url && new URL(response.url).origin !== location.origin) throw new Error('browser.fetch_cross_origin: the request was redirected to another origin, so its response is not returned');
    const reader = response.body ? response.body.getReader() : null;
    const decoder = new TextDecoder();
    let text = '', bytes = 0, truncated = false;
    while (reader) {
      const { done, value } = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      text += decoder.decode(value, { stream: true });
      if (text.length > request.limit) { truncated = true; await reader.cancel(); break; }
    }
    if (!truncated) text += decoder.decode();
    return { status: response.status, status_text: response.statusText, type: response.headers.get('content-type') || '', path: target.pathname + target.search, bytes, truncated, ms: Math.round(performance.now() - started), body: text.slice(0, request.limit) };
  } catch (error) {
    if (error && error.name === 'AbortError') throw new Error('browser.fetch_timeout: the request took longer than 20 seconds');
    throw error;
  } finally {
    clearTimeout(timer);
  }
}
