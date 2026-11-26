// Velarouter web viewer — the reference JavaScript implementation of VRP/1.
//
// It is deliberately dependency-free at runtime except for one optional 51 KB
// vendored bundle (`vendor/noble.js`), used only when WebCrypto is unavailable.
//
// Why a fallback at all: `crypto.subtle` requires a *secure context*. A plain
// `http://192.168.1.10:34789` LAN page is not one, so a browser would refuse the
// handshake. Rather than force TLS with a self-signed certificate warning, the
// viewer ships a pure-JS P-256/HKDF/HMAC/AES-GCM implementation for that case.
//
// Read this together with `docs/PROTOCOL.md` and `docs/sdk/README.md`.

const MAGIC = 0x56524631; // "VRF1"
const ENVELOPE_CONTROL = 1;
const ENVELOPE_MEDIA = 2;
const HKDF_INFO = 'velarouter/vrp1/session-key';
const AAD = new TextEncoder().encode('vrp1');
const DIR_VIEWER_TO_AGENT = 1;
const DIR_AGENT_TO_VIEWER = 2;

const $ = (id) => document.getElementById(id);
const enc = new TextEncoder();
const dec = new TextDecoder();

const b64 = {
  encode(bytes) {
    let s = '';
    for (let i = 0; i < bytes.length; i += 0x8000) {
      s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
    }
    return btoa(s);
  },
  decode(text) {
    const bin = atob(text);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  },
};

const randomHex = (n = 8) =>
  [...crypto.getRandomValues(new Uint8Array(n))].map((b) => b.toString(16).padStart(2, '0')).join('');

// ---------------------------------------------------------------------------
// Crypto backends
// ---------------------------------------------------------------------------

class WebCryptoBackend {
  constructor() {
    this.name = 'webcrypto';
    this.subtle = globalThis.crypto.subtle;
  }
  async generateKeyPair() {
    const kp = await this.subtle.generateKey({ name: 'ECDH', namedCurve: 'P-256' }, false, ['deriveBits']);
    return { priv: kp.privateKey, pub: new Uint8Array(await this.subtle.exportKey('raw', kp.publicKey)) };
  }
  async ecdh(priv, peerPubBytes) {
    const peer = await this.subtle.importKey('raw', peerPubBytes, { name: 'ECDH', namedCurve: 'P-256' }, false, []);
    return new Uint8Array(await this.subtle.deriveBits({ name: 'ECDH', public: peer }, priv, 256));
  }
  async hkdf(shared, salt, info, length = 32) {
    const key = await this.subtle.importKey('raw', shared, 'HKDF', false, ['deriveBits']);
    return new Uint8Array(
      await this.subtle.deriveBits({ name: 'HKDF', hash: 'SHA-256', salt, info }, key, length * 8),
    );
  }
  async hmacKey(keyBytes) {
    return this.subtle.importKey('raw', keyBytes, { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  }
  async hmac(key, bytes) {
    return new Uint8Array(await this.subtle.sign('HMAC', key, bytes));
  }
  async aesKey(keyBytes) {
    return this.subtle.importKey('raw', keyBytes, { name: 'AES-GCM' }, false, ['encrypt', 'decrypt']);
  }
  async encrypt(key, iv, plain) {
    return new Uint8Array(
      await this.subtle.encrypt({ name: 'AES-GCM', iv, additionalData: AAD }, key, plain),
    );
  }
  async decrypt(key, iv, ct) {
    return new Uint8Array(
      await this.subtle.decrypt({ name: 'AES-GCM', iv, additionalData: AAD }, key, ct),
    );
  }
}

class NobleBackend {
  constructor(mod) {
    this.name = 'noble';
    this.mod = mod;
  }
  async generateKeyPair() {
    const priv = this.mod.p256.utils.randomSecretKey();
    return { priv, pub: this.mod.p256.getPublicKey(priv, false) };
  }
  async ecdh(priv, peerPubBytes) {
    // Compressed output is 33 bytes: 0x02/0x03 || X. The protocol uses X only,
    // matching Rust's `raw_secret_bytes()`.
    return this.mod.p256.getSharedSecret(priv, peerPubBytes, true).slice(1);
  }
  async hkdf(shared, salt, info, length = 32) {
    return this.mod.hkdf(this.mod.sha256, shared, salt, info, length);
  }
  async hmacKey(keyBytes) {
    return keyBytes;
  }
  async hmac(keyBytes, bytes) {
    return this.mod.hmac(this.mod.sha256, keyBytes, bytes);
  }
  async aesKey(keyBytes) {
    return keyBytes;
  }
  async encrypt(keyBytes, iv, plain) {
    return this.mod.gcm(keyBytes, iv, AAD).encrypt(plain);
  }
  async decrypt(keyBytes, iv, ct) {
    return this.mod.gcm(keyBytes, iv, AAD).decrypt(ct);
  }
}

async function createCrypto() {
  const forceFallback = new URLSearchParams(location.search).has('fallback');
  if (!forceFallback && globalThis.crypto?.subtle) {
    return new WebCryptoBackend();
  }
  const mod = await import('/vendor/noble.js');
  return new NobleBackend(mod);
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

class AsyncQueue {
  constructor() {
    this.items = [];
    this.waiters = [];
  }
  push(item) {
    const waiter = this.waiters.shift();
    if (waiter) waiter(item);
    else this.items.push(item);
  }
  next() {
    if (this.items.length) return Promise.resolve(this.items.shift());
    return new Promise((resolve) => this.waiters.push(resolve));
  }
}

class VrClient {
  constructor({ code, sessionId, crypto: cryptoBackend, onFrame, onControl, onClose }) {
    this.code = code.trim();
    this.sessionId = sessionId;
    this.crypto = cryptoBackend;
    this.onFrame = onFrame;
    this.onControl = onControl;
    this.onClose = onClose;

    this.queue = new AsyncQueue();
    this.txCounter = 0n;
    this.rxCounter = 0n;
    this.closed = false;
    this.pendingDecode = false;
  }

  async connect() {
    const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
    const url = `${scheme}://${location.host}/vr?sid=${encodeURIComponent(this.sessionId)}`;
    this.ws = new WebSocket(url);
    this.ws.binaryType = 'arraybuffer';

    await new Promise((resolve, reject) => {
      this.ws.onopen = resolve;
      this.ws.onerror = () => reject(new Error('无法连接被控端'));
    });
    this.ws.onmessage = (ev) => this.queue.push(ev.data);
    this.ws.onclose = () => {
      this.closed = true;
      this.onClose?.();
    };

    this.sendJson({
      t: 'hello',
      version: 1,
      role: 'viewer',
      session_id: this.sessionId,
      app: 'webviewer/0.1',
    });

    const keyPair = await this.crypto.generateKeyPair();
    this.sendJson({
      t: 'auth_init',
      viewer_pub: b64.encode(keyPair.pub),
      nonce: b64.encode(crypto.getRandomValues(new Uint8Array(16))),
    });

    const finish = await this.nextJson();
    if (finish.t === 'auth_fail') throw new Error(finish.reason || '被控端拒绝连接');
    if (finish.t !== 'auth_finish') throw new Error('握手协议错误：缺少 auth_finish');

    const shared = await this.crypto.ecdh(keyPair.priv, b64.decode(finish.agent_pub));
    const salt = enc.encode(`${this.sessionId}|${this.code}`);
    this.key = await this.crypto.hkdf(shared, salt, enc.encode(HKDF_INFO), 32);
    this.hmacKey = await this.crypto.hmacKey(this.key);

    const expected = await this.hmac(`agent|${this.sessionId}`);
    if (expected !== finish.proof) throw new Error('被控端身份校验失败（配对码错误？）');

    this.sendJson({ t: 'auth_proof', proof: await this.hmac(`viewer|${this.sessionId}`) });
    const ok = await this.nextJson();
    if (ok.t !== 'auth_ok') throw new Error(ok.reason || '握手失败');

    this.aes = await this.crypto.aesKey(this.key);

    // First encrypted message on both directions is `caps`.
    await this.sendControl({
      t: 'caps',
      capabilities: {
        codecs: ['jpeg'],
        max_width: 2560,
        max_height: 1600,
        max_fps: 60,
        input: true,
        clipboard: true,
        multi_display: true,
        audio: false,
      },
    });

    this.loop();
  }

  async hmac(message) {
    return b64.encode(await this.crypto.hmac(this.hmacKey, enc.encode(message)));
  }

  sendJson(obj) {
    if (this.ws.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(obj));
  }

  async nextJson() {
    for (;;) {
      const data = await this.queue.next();
      if (typeof data === 'string') return JSON.parse(data);
    }
  }

  iv(direction, counter) {
    const iv = new Uint8Array(12);
    iv[0] = direction;
    new DataView(iv.buffer).setBigUint64(4, counter);
    return iv;
  }

  async sendControl(obj) {
    const body = enc.encode(JSON.stringify(obj));
    const plain = new Uint8Array(body.length + 1);
    plain[0] = ENVELOPE_CONTROL;
    plain.set(body, 1);
    await this.sendSealed(plain);
  }

  async sendSealed(plain) {
    const counter = this.txCounter++;
    const ct = await this.crypto.encrypt(this.aes, this.iv(DIR_VIEWER_TO_AGENT, counter), plain);
    const frame = new Uint8Array(8 + ct.length);
    new DataView(frame.buffer).setBigUint64(0, counter);
    frame.set(ct, 8);
    if (this.ws.readyState === WebSocket.OPEN) this.ws.send(frame);
  }

  async open(binary) {
    const buf = new Uint8Array(binary);
    if (buf.length < 8 + 16) throw new Error('密文过短');
    const counter = new DataView(buf.buffer, buf.byteOffset).getBigUint64(0);
    if (counter !== this.rxCounter) {
      throw new Error(`帧计数不连续: 期望 ${this.rxCounter}, 收到 ${counter}`);
    }
    this.rxCounter++;
    return this.crypto.decrypt(this.aes, this.iv(DIR_AGENT_TO_VIEWER, counter), buf.subarray(8));
  }

  async loop() {
    while (!this.closed) {
      const data = await this.queue.next();
      if (typeof data === 'string') continue; // relay control after handshake
      let plain;
      try {
        plain = await this.open(data);
      } catch (err) {
        console.error('解密失败', err);
        this.close('decrypt-error');
        return;
      }
      const kind = plain[0];
      if (kind === ENVELOPE_CONTROL) {
        this.handleControl(JSON.parse(dec.decode(plain.subarray(1))));
      } else if (kind === ENVELOPE_MEDIA) {
        this.handleMedia(plain.subarray(1));
      }
    }
  }

  handleControl(msg) {
    if (msg.t === 'ping') {
      this.sendControl({ t: 'pong', ts: msg.ts });
      return;
    }
    this.onControl?.(msg);
  }

  handleMedia(frame) {
    if (frame.length < 24) return;
    const view = new DataView(frame.buffer, frame.byteOffset, 24);
    if (view.getUint32(0) !== MAGIC) return;
    const codec = view.getUint8(5);
    const frameId = view.getUint32(8);
    const display = view.getUint32(12);
    const width = view.getUint16(16);
    const height = view.getUint16(18);
    const length = view.getUint32(20);
    if (24 + length > frame.length) return;
    if (codec !== 1) return; // only JPEG is defined for the web viewer
    const payload = frame.subarray(24, 24 + length);

    // Never queue decoded frames: a slow tab must drop, not lag further behind.
    if (this.pendingDecode) return;
    this.pendingDecode = true;
    createImageBitmap(new Blob([payload], { type: 'image/jpeg' }))
      .then((bitmap) => {
        this.pendingDecode = false;
        this.onFrame?.({ bitmap, width, height, display, frameId });
      })
      .catch(() => {
        this.pendingDecode = false;
      });
  }

  close(reason = 'bye') {
    if (this.closed) return;
    this.closed = true;
    this.sendControl({ t: 'bye', reason }).catch(() => {});
    setTimeout(() => this.ws?.close(), 60);
  }
}

// ---------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------

const canvas = $('screen');
const ctx = canvas.getContext('2d', { alpha: false, desynchronized: true });
const overlay = $('overlay');
const gate = $('gate');
const statusCard = $('status-card');
const gateError = $('gate-error');

let client = null;
let cryptoBackend = null;
let sessionId = randomHex(8);
let quality = 70;
let actualSize = false;
let stats = { fps: 0, kbps: 0, rtt: 0, width: 0, height: 0 };
let displays = [];
let currentDisplay = 0;
let pressedKeys = new Set();
let lastFrameAt = 0;

function setStatus(title, text, retry = false) {
  overlay.classList.remove('hidden');
  gate.classList.add('hidden');
  statusCard.classList.remove('hidden');
  $('status-title').textContent = title;
  $('status-text').textContent = text;
  $('btn-retry').classList.toggle('hidden', !retry);
}

function showGate(error = '') {
  overlay.classList.remove('hidden');
  statusCard.classList.add('hidden');
  gate.classList.remove('hidden');
  gateError.textContent = error;
  $('code').focus();
}

async function start(code) {
  setStatus('正在握手…', '正在与设备建立端到端加密通道');
  if (!cryptoBackend) {
    cryptoBackend = await createCrypto();
    console.info(`[velarouter] crypto backend: ${cryptoBackend.name}`);
  }
  client = new VrClient({
    code,
    sessionId: (sessionId = randomHex(8)),
    crypto: cryptoBackend,
    onFrame: drawFrame,
    onControl: handleControl,
    onClose: () => {
      if (overlay.classList.contains('hidden')) {
        setStatus('连接已断开', '被控端关闭了会话，或网络中断。', true);
      }
    },
  });
  try {
    await client.connect();
    overlay.classList.add('hidden');
    canvas.focus();
    sessionStorage.setItem('vr.code', code);
    requestAnimationFrame(tickStats);
  } catch (err) {
    console.error(err);
    showGate(err.message || String(err));
  }
}

function drawFrame({ bitmap, width, height }) {
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
    $('resolution').textContent = `${width} × ${height}`;
  }
  ctx.drawImage(bitmap, 0, 0, width, height);
  bitmap.close?.();
  lastFrameAt = performance.now();
}

function handleControl(msg) {
  switch (msg.t) {
    case 'display_list':
      displays = msg.displays ?? [];
      renderDisplays();
      break;
    case 'stats':
      stats.fps = msg.fps ?? 0;
      stats.kbps = msg.kbps ?? 0;
      stats.rtt = msg.rtt_ms ?? 0;
      break;
    case 'warning':
      $('stat').textContent = msg.message;
      break;
    case 'error':
      setStatus('被控端报错', msg.message, true);
      break;
    case 'clipboard':
      if (msg.direction === 'agent_to_viewer') navigator.clipboard?.writeText(msg.text).catch(() => {});
      break;
    default:
      break;
  }
}

function renderDisplays() {
  const select = $('display');
  if (!displays.length) {
    select.innerHTML = '';
    return;
  }
  if (!displays.some((d) => d.id === currentDisplay)) {
    currentDisplay = displays.find((d) => d.primary)?.id ?? displays[0].id;
  }
  select.innerHTML = '';
  for (const d of displays) {
    const option = document.createElement('option');
    option.value = String(d.id);
    option.textContent = `${d.name} ${d.width}×${d.height}${d.primary ? ' · 主屏' : ''}`;
    option.selected = d.id === currentDisplay;
    select.append(option);
  }
  const active = displays.find((d) => d.id === currentDisplay);
  if (active) $('device').textContent = `${active.name} · ${active.width}×${active.height}`;
}

function selectDisplay(id) {
  currentDisplay = id;
  const active = displays.find((d) => d.id === id);
  if (active) $('device').textContent = `${active.name} · ${active.width}×${active.height}`;
  client?.sendControl({
    t: 'video_config',
    codec: 'jpeg',
    quality,
    fps: 30,
    max_width: 1920,
    max_height: 1080,
    display: id,
  });
}

function tickStats() {
  if (!client || client.closed) return;
  $('fps').textContent = stats.fps.toFixed(0);
  $('kbps').textContent = stats.kbps.toFixed(0);
  $('rtt').textContent = stats.rtt ? stats.rtt.toFixed(0) : '—';
  const idle = performance.now() - lastFrameAt;
  $('stat').textContent = idle > 2500 && lastFrameAt ? '画面静止（省流）' : '实时';
  requestAnimationFrame(tickStats);
}

// --- input -----------------------------------------------------------------

const norm = (value, extent) => Math.max(0, Math.min(65535, Math.round((value / extent) * 65535)));

function pointFromEvent(ev) {
  const rect = canvas.getBoundingClientRect();
  return {
    x: norm(ev.clientX - rect.left, rect.width),
    y: norm(ev.clientY - rect.top, rect.height),
    display: 0,
  };
}

const buttonName = (n) => ['left', 'middle', 'right', 'back', 'forward'][n] ?? 'left';

function modifiers(ev) {
  return (
    (ev.shiftKey ? 1 : 0) |
    (ev.ctrlKey ? 2 : 0) |
    (ev.altKey ? 4 : 0) |
    (ev.metaKey ? 8 : 0) |
    (ev.getModifierState?.('CapsLock') ? 16 : 0) |
    (ev.getModifierState?.('NumLock') ? 32 : 0)
  );
}

function sendInput(event) {
  client?.sendControl({ t: 'input', event });
}

canvas.addEventListener('mousemove', (ev) => {
  if (!client) return;
  sendInput({ k: 'mouse_move', ...pointFromEvent(ev) });
});

canvas.addEventListener('mousedown', (ev) => {
  ev.preventDefault();
  canvas.focus();
  sendInput({ k: 'mouse_button', button: buttonName(ev.button), down: true, ...pointFromEvent(ev) });
});

window.addEventListener('mouseup', (ev) => {
  if (!client) return;
  sendInput({ k: 'mouse_button', button: buttonName(ev.button), down: false, ...pointFromEvent(ev) });
});

canvas.addEventListener('contextmenu', (ev) => ev.preventDefault());

canvas.addEventListener(
  'wheel',
  (ev) => {
    if (!client) return;
    ev.preventDefault();
    const detents = Math.max(-8, Math.min(8, Math.round(-ev.deltaY / 100) || Math.sign(ev.deltaY)));
    const horizontal = Math.max(-8, Math.min(8, Math.round((ev.shiftKey ? -ev.deltaX : 0) / 100)));
    sendInput({ k: 'wheel', dx: horizontal, dy: detents, ...pointFromEvent(ev) });
  },
  { passive: false },
);

window.addEventListener('keydown', (ev) => {
  if (!client || !overlay.classList.contains('hidden')) return;
  if (ev.key === 'F5' || (ev.metaKey && ev.key === 'r')) return;
  ev.preventDefault();
  if (pressedKeys.has(ev.code)) return;
  pressedKeys.add(ev.code);
  sendInput({ k: 'key', code: ev.code, down: true, modifiers: modifiers(ev) });
});

window.addEventListener('keyup', (ev) => {
  if (!client || !overlay.classList.contains('hidden')) return;
  ev.preventDefault();
  pressedKeys.delete(ev.code);
  sendInput({ k: 'key', code: ev.code, down: false, modifiers: modifiers(ev) });
});

window.addEventListener('blur', () => {
  if (client && pressedKeys.size) {
    pressedKeys.clear();
    sendInput({ k: 'release_all' });
  }
});

canvas.addEventListener('paste', (ev) => {
  const text = ev.clipboardData?.getData('text');
  if (text && client) {
    ev.preventDefault();
    client.sendControl({ t: 'clipboard', direction: 'viewer_to_agent', text });
    sendInput({ k: 'text', text });
  }
});

// Touch: single finger moves the pointer, tap clicks, two fingers scroll.
canvas.addEventListener(
  'touchstart',
  (ev) => {
    ev.preventDefault();
    if (ev.touches.length === 1) {
      const t = ev.touches[0];
      const rect = canvas.getBoundingClientRect();
      sendInput({
        k: 'touch',
        phase: 'start',
        points: [{ id: 0, x: norm(t.clientX - rect.left, rect.width), y: norm(t.clientY - rect.top, rect.height) }],
        display: 0,
      });
    }
  },
  { passive: false },
);

canvas.addEventListener(
  'touchmove',
  (ev) => {
    ev.preventDefault();
    if (ev.touches.length !== 1) return;
    const t = ev.touches[0];
    const rect = canvas.getBoundingClientRect();
    sendInput({
      k: 'touch',
      phase: 'move',
      points: [{ id: 0, x: norm(t.clientX - rect.left, rect.width), y: norm(t.clientY - rect.top, rect.height) }],
      display: 0,
    });
  },
  { passive: false },
);

canvas.addEventListener('touchend', (ev) => {
  ev.preventDefault();
  const rect = canvas.getBoundingClientRect();
  const t = ev.changedTouches[0];
  sendInput({
    k: 'touch',
    phase: 'end',
    points: [{ id: 0, x: norm(t.clientX - rect.left, rect.width), y: norm(t.clientY - rect.top, rect.height) }],
    display: 0,
  });
});

// --- buttons ---------------------------------------------------------------

$('gate-form').addEventListener('submit', (ev) => {
  ev.preventDefault();
  const code = $('code').value.trim();
  if (!code) {
    gateError.textContent = '请输入配对码';
    return;
  }
  gateError.textContent = '';
  start(code);
});

$('btn-retry').addEventListener('click', () => {
  const code = sessionStorage.getItem('vr.code') || '';
  if (code) start(code);
  else showGate();
});

$('btn-disconnect').addEventListener('click', () => {
  client?.close('viewer-closed');
  client = null;
  showGate();
});

$('btn-fit').addEventListener('click', () => {
  actualSize = !actualSize;
  canvas.classList.toggle('actual', actualSize);
  $('btn-fit').textContent = actualSize ? '居中' : '适应';
});

$('btn-full').addEventListener('click', () => {
  if (document.fullscreenElement) document.exitFullscreen();
  else document.documentElement.requestFullscreen().catch(() => {});
});

$('display').addEventListener('change', (ev) => {
  const id = Number(ev.target.value);
  if (Number.isFinite(id)) selectDisplay(id);
});

$('btn-quality').addEventListener('click', () => {
  const levels = [40, 55, 70, 85, 95];
  quality = levels[(levels.indexOf(quality) + 1) % levels.length];
  $('btn-quality').textContent = `画质 ${quality}`;
  client?.sendControl({
    t: 'video_config',
    codec: 'jpeg',
    quality,
    fps: 30,
    max_width: 1920,
    max_height: 1080,
    display: 0,
  });
});

// Pre-fill the code when refreshing a session on the same tab.
const saved = sessionStorage.getItem('vr.code');
if (saved) $('code').value = saved;

// Auto-connect when the URL contains ?code=… (useful for LAN kiosks).
const urlCode = new URLSearchParams(location.search).get('code');
if (urlCode) start(urlCode);
else showGate();

// Keep the session alive; the agent answers with a pong that feeds the RTT readout.
setInterval(() => client?.sendControl({ t: 'ping', ts: Date.now() }), 3000);
