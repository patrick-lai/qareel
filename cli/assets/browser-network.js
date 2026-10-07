((mode) => {
  const key = '__commissionNetwork';
  if (!window[key]) {
    const log = [];
    const track = { inflight: 0, last: performance.now() };
    const begin = () => { track.inflight++; track.last = performance.now(); };
    const end = () => { track.inflight = Math.max(0, track.inflight - 1); track.last = performance.now(); };
    if (typeof PerformanceObserver === 'function') {
      try { new PerformanceObserver(() => { track.last = performance.now(); }).observe({ type: 'resource', buffered: false }); } catch (error) {}
    }
    const push = (entry) => {
      end();
      log.push(entry);
      if (log.length > 500) log.shift();
    };
    const originalFetch = window.fetch;
    if (typeof originalFetch === 'function') {
      window.fetch = function (input, init) {
        const url = String(typeof input === 'string' ? input : (input && input.url) || input);
        const method = String((init && init.method) || (input && input.method) || 'GET').toUpperCase();
        const started = performance.now();
        const elapsed = () => Math.round(performance.now() - started);
        begin();
        return originalFetch.apply(this, arguments).then(
          (response) => {
            push({ method, url, status: response.status, status_text: response.statusText, kind: 'fetch', ms: elapsed() });
            return response;
          },
          (error) => {
            push({ method, url, status: null, failure: String((error && error.message) || error), kind: 'fetch', ms: elapsed() });
            throw error;
          }
        );
      };
    }
    const open = XMLHttpRequest.prototype.open;
    const send = XMLHttpRequest.prototype.send;
    XMLHttpRequest.prototype.open = function (method, url) {
      this.__commission = { method: String(method).toUpperCase(), url: String(url) };
      return open.apply(this, arguments);
    };
    XMLHttpRequest.prototype.send = function () {
      const info = this.__commission;
      if (info) {
        begin();
        const started = performance.now();
        this.addEventListener('loadend', () => {
          const failed = this.status === 0;
          push({ ...info, status: failed ? null : this.status, status_text: failed ? '' : this.statusText, failure: failed ? 'failed' : undefined, kind: 'xhr', ms: Math.round(performance.now() - started) });
        });
      }
      return send.apply(this, arguments);
    };
    window[key] = { log, track, installed: Math.round(performance.timeOrigin + performance.now()) };
  }
  const state = window[key];
  if (mode === 'idle') return state.track.inflight === 0 && performance.now() - state.track.last >= 500 && document.readyState === 'complete';
  const resources = performance.getEntriesByType('resource').map((entry) => ({
    method: 'GET',
    url: entry.name,
    status: entry.responseStatus || null,
    kind: entry.initiatorType,
    ms: Math.round(entry.duration),
    timing: true,
  }));
  return { captured: state.log.slice(), resources, installed: state.installed };
})(__MODE__)
