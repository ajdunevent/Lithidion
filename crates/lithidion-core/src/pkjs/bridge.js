// What a PebbleKit JS script expects to find around it, run before the script in a bare
// JavaScriptCore context. Everything that leaves the context goes through `__native(op, json)`;
// everything coming in arrives through `__deliver(kind, json)`.
(function (global) {
  'use strict';

  const native = global.__native;
  delete global.__native;
  const call = (op, arg) => {
    const out = native(op, arg === undefined ? '' : JSON.stringify(arg, plain));
    return out === undefined || out === '' ? undefined : JSON.parse(out);
  };
  // Byte arrays travel as plain arrays.
  function plain(key, value) {
    if (value instanceof ArrayBuffer) return Array.from(new Uint8Array(value));
    if (ArrayBuffer.isView(value)) return Array.from(new Uint8Array(value.buffer, value.byteOffset, value.byteLength));
    return value;
  }
  const later = (f) => global.setTimeout(f, 0);

  global.window = global;
  global.self = global;

  // Console.
  const describe = (v) => {
    if (typeof v === 'string') return v;
    if (v instanceof Error) return v.stack ? `${v}\n${v.stack}` : String(v);
    try {
      const s = JSON.stringify(v, plain);
      return s === undefined ? String(v) : s;
    } catch (e) {
      return String(v);
    }
  };
  const logger = (level) => (...args) => call('log', { level, text: args.map(describe).join(' ') });
  global.console = {
    log: logger('log'),
    info: logger('info'),
    warn: logger('warn'),
    error: logger('error'),
    debug: logger('debug'),
    trace: logger('debug'),
    assert: (ok, ...args) => { if (!ok) logger('error')('Assertion failed:', ...args); },
  };

  // Timers.
  const timers = new Map();
  let nextTimer = 1;
  const addTimer = (fn, ms, args, repeat) => {
    if (typeof fn !== 'function') fn = new Function(String(fn));
    const id = nextTimer++;
    timers.set(id, { fn, args, repeat });
    call('timer', { id, ms: Math.max(0, Math.floor(Number(ms) || 0)), repeat });
    return id;
  };
  global.setTimeout = (fn, ms, ...args) => addTimer(fn, ms, args, false);
  global.setInterval = (fn, ms, ...args) => addTimer(fn, ms, args, true);
  global.clearTimeout = global.clearInterval = (id) => {
    if (timers.delete(id)) call('cancel', id);
  };

  // Base64.
  const ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  global.btoa = (input) => {
    const s = String(input);
    let out = '';
    for (let i = 0; i < s.length; i += 3) {
      const a = s.charCodeAt(i), b = s.charCodeAt(i + 1), c = s.charCodeAt(i + 2);
      if (a > 255 || b > 255 || c > 255) throw new Error('btoa: the string has characters outside Latin-1');
      const n = (a << 16) | ((b || 0) << 8) | (c || 0);
      out += ALPHABET[n >> 18] + ALPHABET[(n >> 12) & 63] +
        (i + 1 < s.length ? ALPHABET[(n >> 6) & 63] : '=') + (i + 2 < s.length ? ALPHABET[n & 63] : '=');
    }
    return out;
  };
  global.atob = (input) => {
    const s = String(input).replace(/[\s=]/g, '');
    let out = '';
    for (let i = 0; i < s.length; i += 4) {
      const n = [0, 1, 2, 3].map((k) => (i + k < s.length ? ALPHABET.indexOf(s[i + k]) : 0));
      if (n.some((x) => x < 0)) throw new Error('atob: not base64');
      const v = (n[0] << 18) | (n[1] << 12) | (n[2] << 6) | n[3];
      out += String.fromCharCode(v >> 16);
      if (i + 2 < s.length) out += String.fromCharCode((v >> 8) & 255);
      if (i + 3 < s.length) out += String.fromCharCode(v & 255);
    }
    return out;
  };
  const bytesOf = (base64) => {
    const s = global.atob(base64 || '');
    const b = new Uint8Array(s.length);
    for (let i = 0; i < s.length; i++) b[i] = s.charCodeAt(i);
    return b;
  };

  // Storage, kept per app and profile.
  const storage = {
    getItem: (key) => { const v = call('get', String(key)); return v === undefined ? null : v; },
    setItem: (key, value) => { call('set', { key: String(key), value: String(value) }); },
    removeItem: (key) => { call('remove', String(key)); },
    clear: () => { call('clear'); },
    key: (i) => { const k = call('keys'); return i < k.length ? k[i] : null; },
  };
  global.localStorage = new Proxy(storage, {
    get: (t, p) => {
      if (p === 'length') return call('keys').length;
      if (typeof p !== 'string' || p in t) return t[p];
      const v = t.getItem(p);
      return v === null ? undefined : v;
    },
    set: (t, p, v) => { t.setItem(p, v); return true; },
    deleteProperty: (t, p) => { t.removeItem(p); return true; },
    has: (t, p) => p in t || t.getItem(p) !== null,
    ownKeys: () => call('keys'),
    getOwnPropertyDescriptor: (t, p) => {
      const v = t.getItem(p);
      return v === null ? undefined : { value: v, writable: true, enumerable: true, configurable: true };
    },
  });

  // Network. Requests leave through Lithidion, which answers through `deliver('http')`.
  const pending = new Map();
  let nextRequest = 1;
  const headerList = (h) => {
    if (!h) return [];
    if (typeof h.forEach === 'function' && !Array.isArray(h)) {
      const out = [];
      h.forEach((v, k) => out.push([String(k), String(v)]));
      return out;
    }
    if (Array.isArray(h)) return h.map(([k, v]) => [String(k), String(v)]);
    return Object.keys(h).map((k) => [k, String(h[k])]);
  };
  const bodyOf = (body) => {
    if (body === undefined || body === null) return {};
    if (body instanceof ArrayBuffer || ArrayBuffer.isView(body)) {
      const b = body instanceof ArrayBuffer ? new Uint8Array(body) : new Uint8Array(body.buffer, body.byteOffset, body.byteLength);
      let s = '';
      for (const x of b) s += String.fromCharCode(x);
      return { base64: global.btoa(s) };
    }
    return { text: typeof body === 'string' ? body : String(body) };
  };

  class XMLHttpRequest {
    constructor() {
      this.readyState = 0;
      this.status = 0;
      this.statusText = '';
      this.responseText = '';
      this.response = '';
      this.responseType = '';
      this.responseURL = '';
      this.timeout = 0;
      this.withCredentials = false;
      this.onreadystatechange = this.onload = this.onerror = this.ontimeout = this.onabort = this.onloadend = this.onloadstart = this.onprogress = null;
      this._listeners = {};
      this._headers = [];
      this._replyHeaders = [];
    }
    open(method, url, async) {
      this._method = String(method || 'GET').toUpperCase();
      this._url = String(url);
      this._async = async !== false;
      this._headers = [];
      this._id = 0;
      this._set(XMLHttpRequest.OPENED);
    }
    setRequestHeader(name, value) { this._headers.push([String(name), String(value)]); }
    overrideMimeType() {}
    getResponseHeader(name) {
      const n = String(name).toLowerCase();
      const found = this._replyHeaders.filter(([k]) => k.toLowerCase() === n).map(([, v]) => v);
      return found.length ? found.join(', ') : null;
    }
    getAllResponseHeaders() { return this._replyHeaders.map(([k, v]) => `${k}: ${v}\r\n`).join(''); }
    addEventListener(type, f) { (this._listeners[type] = this._listeners[type] || []).push(f); }
    removeEventListener(type, f) { this._listeners[type] = (this._listeners[type] || []).filter((x) => x !== f); }
    dispatchEvent(e) { this._fire(e.type); return true; }
    _fire(type) {
      const e = { type, target: this, currentTarget: this, loaded: 0, total: 0, lengthComputable: false };
      const handlers = [this['on' + type], ...(this._listeners[type] || [])];
      for (const h of handlers) {
        if (typeof h !== 'function') continue;
        try { h.call(this, e); } catch (err) { console.error(err); }
      }
    }
    _set(state) {
      this.readyState = state;
      this._fire('readystatechange');
    }
    send(body) {
      const request = {
        method: this._method, url: this._url, headers: this._headers, timeout: this.timeout,
        binary: this.responseType === 'arraybuffer' || this.responseType === 'blob',
        ...bodyOf(this._method === 'GET' || this._method === 'HEAD' ? null : body),
      };
      this._fire('loadstart');
      if (!this._async) {
        this._finish(call('http', request));
        return;
      }
      this._id = nextRequest++;
      pending.set(this._id, (reply) => this._finish(reply));
      call('http', { id: this._id, ...request });
    }
    abort() {
      if (this._id) pending.delete(this._id);
      this._id = 0;
      if (this.readyState !== XMLHttpRequest.UNSENT && this.readyState !== XMLHttpRequest.DONE) {
        this.status = 0;
        this._set(XMLHttpRequest.DONE);
        this._fire('abort');
        this._fire('loadend');
      }
      this.readyState = XMLHttpRequest.UNSENT;
    }
    _finish(reply) {
      this._id = 0;
      if (reply.error) {
        this.status = 0;
        this.statusText = '';
        this._set(XMLHttpRequest.DONE);
        this._fire(reply.timedOut ? 'timeout' : 'error');
        this._fire('loadend');
        return;
      }
      this.status = reply.status;
      this.statusText = reply.statusText;
      this.responseURL = reply.url;
      this._replyHeaders = reply.headers;
      this._set(XMLHttpRequest.HEADERS_RECEIVED);
      this._set(XMLHttpRequest.LOADING);
      this.responseText = reply.text || '';
      switch (this.responseType) {
        case 'arraybuffer': this.response = bytesOf(reply.base64).buffer; break;
        case 'blob': this.response = bytesOf(reply.base64); break;
        case 'json':
          try { this.response = JSON.parse(this.responseText); } catch (e) { this.response = null; }
          break;
        default: this.response = this.responseText;
      }
      this._set(XMLHttpRequest.DONE);
      this._fire('load');
      this._fire('loadend');
    }
  }
  Object.assign(XMLHttpRequest, { UNSENT: 0, OPENED: 1, HEADERS_RECEIVED: 2, LOADING: 3, DONE: 4 });
  global.XMLHttpRequest = XMLHttpRequest;

  global.fetch = (input, init) => new Promise((resolve, reject) => {
    const options = init || {};
    const url = typeof input === 'string' ? input : String(input.url || input);
    const method = String(options.method || 'GET').toUpperCase();
    const id = nextRequest++;
    pending.set(id, (reply) => {
      if (reply.error) {
        reject(new TypeError(reply.error));
        return;
      }
      const text = reply.text || '';
      resolve({
        ok: reply.status >= 200 && reply.status < 300,
        status: reply.status,
        statusText: reply.statusText,
        url: reply.url,
        headers: {
          get: (n) => {
            const k = String(n).toLowerCase();
            const v = reply.headers.filter(([h]) => h.toLowerCase() === k).map(([, x]) => x);
            return v.length ? v.join(', ') : null;
          },
          has: (n) => reply.headers.some(([h]) => h.toLowerCase() === String(n).toLowerCase()),
          forEach: (f) => reply.headers.forEach(([k, v]) => f(v, k)),
        },
        text: () => Promise.resolve(text),
        json: () => Promise.resolve().then(() => JSON.parse(text)),
        arrayBuffer: () => Promise.resolve(bytesOf(reply.base64).buffer),
      });
    });
    call('http', {
      id, method, url, headers: headerList(options.headers), timeout: 0, binary: true,
      ...bodyOf(method === 'GET' || method === 'HEAD' ? null : options.body),
    });
  });

  // WebSockets, also through Lithidion; their events arrive through `deliver('socket')`.
  const sockets = new Map();
  let nextSocket = 1;
  const domError = (name, message) => { const e = new Error(message); e.name = name; return e; };
  class WebSocket {
    constructor(url, protocols) {
      const list = protocols === undefined ? [] : (Array.isArray(protocols) ? protocols : [protocols]).map(String);
      const id = nextSocket++;
      const opened = call('socket', { id, url: String(url), protocols: list });
      if (opened && opened.error) throw new SyntaxError(opened.error);
      this._id = id;
      this.url = opened.url;
      this.protocol = '';
      this.extensions = '';
      this.readyState = WebSocket.CONNECTING;
      this.bufferedAmount = 0;
      this.binaryType = 'blob';
      this.onopen = this.onmessage = this.onerror = this.onclose = null;
      this._listeners = {};
      sockets.set(id, this);
    }
    send(data) {
      if (this.readyState === WebSocket.CONNECTING) throw domError('InvalidStateError', 'The socket is still connecting');
      if (this.readyState !== WebSocket.OPEN) return;
      call('socketSend', { id: this._id, ...bodyOf(data === undefined || data === null ? String(data) : data) });
    }
    close(code, reason) {
      if (code !== undefined && code !== 1000 && !(code >= 3000 && code <= 4999)) {
        throw domError('InvalidAccessError', `${code} is not a close code a script may use`);
      }
      if (this.readyState === WebSocket.CLOSING || this.readyState === WebSocket.CLOSED) return;
      this.readyState = WebSocket.CLOSING;
      call('socketClose', { id: this._id, code: code === undefined ? null : code, reason: reason === undefined ? '' : String(reason) });
    }
    addEventListener(type, f) { (this._listeners[type] = this._listeners[type] || []).push(f); }
    removeEventListener(type, f) { this._listeners[type] = (this._listeners[type] || []).filter((x) => x !== f); }
    dispatchEvent(e) { this._fire(e.type, e); return true; }
    _fire(type, extra) {
      const e = Object.assign({ type, target: this, currentTarget: this }, extra);
      const handlers = [this['on' + type], ...(this._listeners[type] || [])];
      for (const h of handlers) {
        if (typeof h !== 'function') continue;
        try { h.call(this, e); } catch (err) { console.error(err); }
      }
    }
    _event(d) {
      switch (d.kind) {
        case 'open':
          this.readyState = WebSocket.OPEN;
          this.protocol = d.protocol || '';
          this._fire('open');
          break;
        case 'message': {
          if (this.readyState !== WebSocket.OPEN) break;
          let data = d.text;
          if (data === undefined) data = this.binaryType === 'arraybuffer' ? bytesOf(d.base64).buffer : bytesOf(d.base64);
          this._fire('message', { data, origin: this.url.replace(/^(wss?:\/\/[^/?#]*).*$/, '$1'), lastEventId: '' });
          break;
        }
        case 'error':
          this._fire('error', { message: d.message });
          break;
        case 'close':
          sockets.delete(this._id);
          this.readyState = WebSocket.CLOSED;
          this._fire('close', { code: d.code, reason: d.reason || '', wasClean: !!d.clean });
          break;
      }
    }
  }
  Object.assign(WebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
  Object.assign(WebSocket.prototype, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
  global.WebSocket = WebSocket;

  // Location.
  const located = new Map();
  let nextFix = 1;
  const locate = (ok, fail, options, watch) => {
    const id = nextFix++;
    located.set(id, { ok, fail, watch });
    call('location', { id, timeout: options && options.timeout > 0 ? options.timeout : 0 });
    return id;
  };
  global.navigator = {
    userAgent: 'Lithidion',
    language: String((call('watch') || {}).language || 'en_US').replace('_', '-'),
    onLine: true,
    geolocation: {
      getCurrentPosition: (ok, fail, options) => { locate(ok, fail, options, false); },
      watchPosition: (ok, fail, options) => locate(ok, fail, options, true),
      clearWatch: (id) => { located.delete(id); },
    },
  };

  // Pebble.
  const listeners = new Map();
  const emit = (type, detail) => {
    const e = Object.assign({ type, bubbles: false, cancelable: false }, detail);
    for (const f of [...(listeners.get(type) || [])]) {
      try { f.call(global, e); } catch (err) { console.error(`${type} listener:`, err); }
    }
  };
  const sent = new Map();
  const unavailable = (what) => (...args) => {
    const fail = args.filter((a) => typeof a === 'function')[1];
    if (fail) later(() => fail(`${what} is not available`));
  };
  global.Pebble = {
    addEventListener: (type, f) => {
      if (typeof f !== 'function') return;
      if (!listeners.has(type)) listeners.set(type, new Set());
      listeners.get(type).add(f);
    },
    removeEventListener: (type, f) => { if (listeners.has(type)) listeners.get(type).delete(f); },
    sendAppMessage: (data, ack, nack) => {
      const id = call('send', data || {});
      if (id < 0) {
        if (nack) later(() => nack({ data: { transactionId: id }, error: { message: 'The watch is not connected' } }));
        return id;
      }
      if (ack || nack) sent.set(id, { ack, nack });
      return id;
    },
    showSimpleNotificationOnPebble: (title, body) => { call('notify', { title: String(title), body: String(body) }); },
    getAccountToken: () => call('token', 'account'),
    getWatchToken: () => call('token', 'watch'),
    getActiveWatchInfo: () => call('watch'),
    openURL: (url) => { call('open', String(url)); },
    getTimelineToken: unavailable('A timeline token'),
    timelineSubscribe: unavailable('Timeline subscription'),
    timelineUnsubscribe: unavailable('Timeline subscription'),
    timelineSubscriptions: unavailable('Timeline subscription'),
    appGlanceReload: unavailable('App glance'),
  };

  global.__deliver = (kind, json) => {
    const d = json ? JSON.parse(json) : undefined;
    switch (kind) {
      case 'ready': emit('ready', { ready: true }); break;
      case 'appmessage': emit('appmessage', { payload: d }); break;
      case 'ack':
      case 'nack': {
        const s = sent.get(d);
        sent.delete(d);
        const f = s && (kind === 'ack' ? s.ack : s.nack);
        if (f) {
          const e = { data: { transactionId: d } };
          if (kind === 'nack') e.error = { message: 'The watch did not take the message' };
          try { f(e); } catch (err) { console.error(err); }
        }
        break;
      }
      case 'showConfiguration': emit('showConfiguration', {}); break;
      case 'webviewclosed': emit('webviewclosed', { response: d }); break;
      case 'timer': {
        const t = timers.get(d);
        if (!t) break;
        if (!t.repeat) timers.delete(d);
        try { t.fn.apply(global, t.args); } catch (err) { console.error(err); }
        break;
      }
      case 'http': {
        const f = pending.get(d.id);
        pending.delete(d.id);
        if (f) f(d);
        break;
      }
      case 'socket': {
        const s = sockets.get(d.id);
        if (s) s._event(d);
        break;
      }
      case 'location': {
        const l = located.get(d.id);
        if (!l) break;
        if (!l.watch) located.delete(d.id);
        try {
          if (d.error) { if (l.fail) l.fail(d.error); } else if (l.ok) l.ok({ coords: d.coords, timestamp: d.timestamp });
        } catch (err) { console.error(err); }
        break;
      }
    }
  };
})(globalThis);
