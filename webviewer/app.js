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
    this.closed = false;
  }
  push(item) {
    if (this.closed) return;
    const waiter = this.waiters.shift();
    if (waiter) waiter(item);
    else this.items.push(item);
  }
  next() {
    if (this.closed) return Promise.resolve(null);
    if (this.items.length) return Promise.resolve(this.items.shift());
    return new Promise((resolve) => this.waiters.push(resolve));
  }
  close() {
    this.closed = true;
    this.items.length = 0;
    for (const resolve of this.waiters.splice(0)) resolve(null);
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
    this.outgoing = Promise.resolve();
  }

  async connect() {
    const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
    const url = `${scheme}://${location.host}/vr?sid=${encodeURIComponent(this.sessionId)}`;
    this.ws = new WebSocket(url);
    this.ws.binaryType = 'arraybuffer';

    this.ws.onmessage = (ev) => this.queue.push(ev.data);
    await new Promise((resolve, reject) => {
      this.ws.onopen = resolve;
      this.ws.onerror = () => reject(new Error('无法连接被控端'));
      this.ws.onclose = () => {
        const wasClosed = this.closed;
        this.closed = true;
        this.queue.close();
        reject(new Error('连接已关闭'));
        if (!wasClosed) this.onClose?.();
      };
    });

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
    if (ok.reason === 'trusted viewer required') {
      throw new Error('对方仅允许可信设备。请使用已配对的桌面客户端。');
    }
    if (ok.t !== 'auth_ok') throw new Error(ok.reason || '握手失败');
    this.code = '';

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

    if (this.closed) throw new Error('连接已关闭');
    this.loop().catch(() => this.abort('会话数据无效，连接已关闭。'));
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
      if (data === null) throw new Error('握手时连接已关闭');
      if (typeof data === 'string') return JSON.parse(data);
    }
  }

  iv(direction, counter) {
    const iv = new Uint8Array(12);
    iv[0] = direction;
    new DataView(iv.buffer).setBigUint64(4, counter);
    return iv;
  }

  sendControl(obj) {
    if (!this.aes || this.closed) return Promise.resolve();
    const body = enc.encode(JSON.stringify(obj));
    const plain = new Uint8Array(body.length + 1);
    plain[0] = ENVELOPE_CONTROL;
    plain.set(body, 1);
    // WebCrypto encryption can finish out of order. Allocate/send counters
    // serially so pointer down/move/up and release-all stay ordered on the wire.
    this.outgoing = this.outgoing.then(() => this.sendSealed(plain)).catch(() => {
      this.abort('无法发送会话数据，连接已关闭。');
    });
    return this.outgoing;
  }

  async sendSealed(plain) {
    if (this.closed || this.ws.readyState !== WebSocket.OPEN) return;
    const counter = this.txCounter++;
    const ct = await this.crypto.encrypt(this.aes, this.iv(DIR_VIEWER_TO_AGENT, counter), plain);
    const frame = new Uint8Array(8 + ct.length);
    new DataView(frame.buffer).setBigUint64(0, counter);
    frame.set(ct, 8);
    if (!this.closed && this.ws.readyState === WebSocket.OPEN) this.ws.send(frame);
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
      if (data === null || this.closed) return;
      if (typeof data === 'string') continue; // relay control after handshake
      let plain;
      try {
        plain = await this.open(data);
      } catch (err) {
        console.error('解密失败', err);
        this.abort('会话校验失败，连接已关闭。');
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
    if (24 + length !== frame.length || length > 8 * 1024 * 1024) return;
    if (!width || !height || width > 4096 || height > 4096 || codec !== 1) return;
    const payload = frame.subarray(24, 24 + length);

    // Never queue decoded frames: a slow tab must drop, not lag further behind.
    if (this.pendingDecode) return;
    this.pendingDecode = true;
    decodeJpeg(new Blob([payload], { type: 'image/jpeg' }))
      .then((bitmap) => {
        this.pendingDecode = false;
        if (this.closed || !this.onFrame || bitmap.width !== width || bitmap.height !== height) {
          bitmap.close?.();
          return;
        }
        this.onFrame({ bitmap, width, height, display, frameId });
      })
      .catch(() => {
        this.pendingDecode = false;
      });
  }

  abort(message) {
    if (this.closed) return;
    this.closed = true;
    this.code = '';
    this.queue.close();
    this.ws?.close();
    if (message) this.onClose?.(message);
  }

  close(reason = 'bye') {
    if (this.closed) return;
    if (!this.aes) return this.abort();
    this.sendControl({ t: 'bye', reason }).finally(() => this.abort());
    setTimeout(() => this.abort(), 400);
  }
}

// Older mobile browsers may lack createImageBitmap. Blob URLs are local and
// are revoked as soon as the image finishes decoding.
async function decodeJpeg(blob) {
  if (globalThis.createImageBitmap) return createImageBitmap(blob);
  const url = URL.createObjectURL(blob);
  try {
    const image = new Image();
    await new Promise((resolve, reject) => {
      image.onload = resolve;
      image.onerror = reject;
      image.src = url;
    });
    return image;
  } finally {
    URL.revokeObjectURL(url);
  }
}

// ---------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------

const canvas = $('screen');
const viewport = $('viewport');
const ctx = canvas.getContext('2d', { alpha: false, desynchronized: true });
const overlay = $('overlay');
const gate = $('gate');
const statusCard = $('status-card');
const gateError = $('gate-error');

let client = null;
let cryptoBackend = null;
let connecting = false;
let quality = 70;
let stats = { fps: 0, kbps: 0, rtt: 0 };
let displays = [];
let currentDisplay = 0;
const pressedKeys = new Set();
const heldButtons = new Set();
let activePointer = null;
let firstFrame = false;
let frameDisplay = 0;
let pendingDisplay = null;
let actualMode = false;
let statsLoop = 0;

function setStatus(title, text, retry = false) {
  canvas.inert = true;
  setSessionControls(false);
  overlay.classList.remove('hidden');
  gate.classList.add('hidden');
  statusCard.classList.remove('hidden');
  $('status-title').textContent = title;
  $('status-text').textContent = text;
  $('btn-retry').classList.toggle('hidden', !retry);
  if (retry) $('btn-retry').focus();
}

function showGate(error = '', focus = true) {
  canvas.inert = true;
  setSessionControls(false);
  overlay.classList.remove('hidden');
  statusCard.classList.add('hidden');
  gate.classList.remove('hidden');
  gateError.textContent = error;
  if (focus) $('code').focus();
}

function setActualMode(enabled) {
  releaseInputs();
  actualMode = enabled;
  $('btn-fit').setAttribute('aria-pressed', String(enabled));
  $('btn-fit').textContent = enabled ? '返回操控' : '原始尺寸';
  $('stat').textContent = enabled ? '原始尺寸浏览 · 不发送操作' : '';
  canvas.classList.toggle('inspect', enabled);
  sizeCanvas();
  viewport.scrollTo(0, 0);
}

function setSessionControls(enabled) {
  for (const id of ['display', 'btn-quality', 'btn-fit', 'btn-full', 'btn-disconnect']) {
    $(id).disabled = !enabled;
  }
}

async function start(code) {
  if (connecting || client) return;
  connecting = true;
  firstFrame = false;
  frameDisplay = 0;
  pendingDisplay = null;
  currentDisplay = 0;
  displays = [];
  stats = { fps: 0, kbps: 0, rtt: 0 };
  renderDisplays();
  $('device').textContent = '连接中…';
  setActualMode(false);
  $('resolution').textContent = '—';
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  setStatus('正在握手…', '正在与设备建立端到端加密通道');
  let attempt;
  let timeout;
  try {
    cryptoBackend ??= await createCrypto();
    attempt = new VrClient({
      code,
      sessionId: randomHex(8),
      crypto: cryptoBackend,
      onFrame: (frame) => {
        if (client === attempt && !attempt.closed) drawFrame(frame);
        else frame.bitmap.close?.();
      },
      onControl: (msg) => { if (client === attempt) handleControl(msg); },
      onClose: (message) => {
        if (client === attempt && !connecting) finishConnection(message);
      },
    });
    client = attempt;
    await Promise.race([
      attempt.connect(),
      new Promise((_, reject) => {
        timeout = setTimeout(() => reject(new Error('连接超时，请检查被控端和网络。')), 12000);
      }),
    ]);
    if (attempt.closed) throw new Error('连接已关闭');
    overlay.classList.add('hidden');
    canvas.inert = false;
    if (!displays.length) $('device').textContent = '已连接';
    setSessionControls(true);
    if (matchMedia('(pointer: fine)').matches) canvas.focus({ preventScroll: true });
    statsLoop = requestAnimationFrame(tickStats);
  } catch (err) {
    attempt?.abort();
    if (client === attempt) client = null;
    $('device').textContent = '未连接';
    showGate(err.message || String(err));
  } finally {
    clearTimeout(timeout);
    connecting = false;
  }
}

function finishConnection(message, notify = true) {
  const active = client;
  releaseInputs();
  client = null;
  active?.close();
  cancelAnimationFrame(statsLoop);
  firstFrame = false;
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  $('device').textContent = '未连接';
  $('stat').textContent = '';
  $('resolution').textContent = '—';
  if (notify) setStatus('连接已断开', message || '被控端关闭了会话，或网络中断。请重新输入密码连接。', true);
}

function sizeCanvas() {
  const { paddingLeft, paddingRight } = getComputedStyle(viewport);
  const availableWidth = Math.max(0, viewport.clientWidth - parseFloat(paddingLeft) - parseFloat(paddingRight));
  const scale = actualMode ? 1 : Math.min(availableWidth / canvas.width, viewport.clientHeight / canvas.height, 1);
  canvas.style.width = `${canvas.width * scale}px`;
  canvas.style.height = `${canvas.height * scale}px`;
}
new ResizeObserver(sizeCanvas).observe(viewport);

function drawFrame({ bitmap, width, height, display }) {
  if (pendingDisplay !== null) {
    if (display !== pendingDisplay) {
      bitmap.close?.();
      return;
    }
    pendingDisplay = null;
  }
  if (frameDisplay !== display) releaseInputs();
  firstFrame = true;
  frameDisplay = display;
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
    sizeCanvas();
  }
  $('resolution').textContent = `${width} × ${height}`;
  ctx.drawImage(bitmap, 0, 0, width, height);
  bitmap.close?.();
}

function handleControl(msg) {
  switch (msg.t) {
    case 'display_list':
      displays = msg.displays ?? [];
      if (pendingDisplay !== null && !displays.some((display) => display.id === pendingDisplay)) pendingDisplay = null;
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
      finishConnection(msg.message || '被控端发生错误。');
      break;
    case 'bye':
      finishConnection('被控端结束了会话。');
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
  if (id !== currentDisplay) {
    releaseInputs();
    pendingDisplay = id;
    firstFrame = false;
  }
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
  statsLoop = requestAnimationFrame(tickStats);
}

// --- input -----------------------------------------------------------------

const norm = (value, extent) => Math.max(0, Math.min(65535, Math.round((value / Math.max(extent, 1)) * 65535)));

function pointFromEvent(ev) {
  const rect = canvas.getBoundingClientRect();
  return {
    x: norm(ev.clientX - rect.left, rect.width),
    y: norm(ev.clientY - rect.top, rect.height),
    display: frameDisplay,
  };
}

const buttonName = (n) => ['left', 'middle', 'right', 'back', 'forward'][n];

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

function canSendInput() {
  return client && !client.closed && firstFrame && !actualMode && overlay.classList.contains('hidden');
}

function sendInput(event) {
  client?.sendControl({ t: 'input', event });
}

let pointerMove = null;
let pointerFrame = 0;
function flushMove() {
  cancelAnimationFrame(pointerFrame);
  pointerFrame = 0;
  if (pointerMove && canSendInput()) sendInput({ k: 'mouse_move', ...pointerMove });
  pointerMove = null;
}

function releaseInputs() {
  cancelAnimationFrame(pointerFrame);
  pointerFrame = 0;
  pointerMove = null;
  const pointer = activePointer;
  activePointer = null;
  if (heldButtons.size || pressedKeys.size) sendInput({ k: 'release_all' });
  heldButtons.clear();
  pressedKeys.clear();
  if (pointer !== null && canvas.hasPointerCapture(pointer)) canvas.releasePointerCapture(pointer);
}

canvas.addEventListener('pointerdown', (ev) => {
  if (!canSendInput() || (ev.pointerType !== 'mouse' && !ev.isPrimary)) return;
  const button = buttonName(ev.button);
  if (!button || activePointer !== null) return;
  ev.preventDefault();
  canvas.focus({ preventScroll: true });
  flushMove();
  activePointer = ev.pointerId;
  canvas.setPointerCapture(ev.pointerId);
  heldButtons.add(button);
  sendInput({ k: 'mouse_button', button, down: true, ...pointFromEvent(ev) });
});

canvas.addEventListener('pointermove', (ev) => {
  if (!canSendInput() || (activePointer !== null && activePointer !== ev.pointerId)) return;
  if (ev.pointerType !== 'mouse' && activePointer !== ev.pointerId) return;
  pointerMove = pointFromEvent(ev);
  if (!pointerFrame) pointerFrame = requestAnimationFrame(flushMove);
});

canvas.addEventListener('pointerup', (ev) => {
  if (activePointer !== ev.pointerId) return;
  flushMove();
  // Pointer Events only emits pointerup for the last button on a mouse.
  for (const button of heldButtons) {
    sendInput({ k: 'mouse_button', button, down: false, ...pointFromEvent(ev) });
  }
  heldButtons.clear();
  activePointer = null;
});

for (const type of ['pointercancel', 'lostpointercapture']) {
  canvas.addEventListener(type, (ev) => { if (activePointer === ev.pointerId) releaseInputs(); });
}
canvas.addEventListener('contextmenu', (ev) => { if (canSendInput()) ev.preventDefault(); });

canvas.addEventListener('wheel', (ev) => {
  if (!canSendInput() || ev.ctrlKey || ev.metaKey) return;
  ev.preventDefault();
  const steps = (value) => Math.max(-8, Math.min(8, Math.round(value / 100) || Math.sign(value)));
  sendInput({ k: 'wheel', dx: steps(-ev.deltaX), dy: steps(-ev.deltaY), ...pointFromEvent(ev) });
}, { passive: false });

canvas.addEventListener('keydown', (ev) => {
  if (!canSendInput() || !ev.code || ev.key === 'Tab' || ev.key === 'F5' || ((ev.metaKey || ev.ctrlKey) && ev.key.toLowerCase() === 'r')) return;
  ev.preventDefault();
  if (pressedKeys.has(ev.code)) return;
  pressedKeys.add(ev.code);
  sendInput({ k: 'key', code: ev.code, down: true, modifiers: modifiers(ev) });
});

canvas.addEventListener('keyup', (ev) => {
  if (!canSendInput() || !pressedKeys.delete(ev.code)) return;
  ev.preventDefault();
  sendInput({ k: 'key', code: ev.code, down: false, modifiers: modifiers(ev) });
});

canvas.addEventListener('blur', releaseInputs);
window.addEventListener('blur', releaseInputs);
document.addEventListener('visibilitychange', () => { if (document.hidden) releaseInputs(); });

canvas.addEventListener('paste', (ev) => {
  const text = ev.clipboardData?.getData('text');
  if (text && canSendInput()) {
    ev.preventDefault();
    client.sendControl({ t: 'clipboard', direction: 'viewer_to_agent', text });
    sendInput({ k: 'text', text });
  }
});

// --- buttons ---------------------------------------------------------------

$('gate-form').addEventListener('submit', (ev) => {
  ev.preventDefault();
  const input = $('code');
  const code = input.value.trim();
  if (!code) {
    gateError.textContent = '请输入连接密码';
    return;
  }
  input.value = '';
  gateError.textContent = '';
  start(code);
});

$('btn-retry').addEventListener('click', () => showGate());
$('btn-disconnect').addEventListener('click', () => {
  finishConnection('', false);
  showGate();
});

$('btn-fit').addEventListener('click', () => setActualMode(!actualMode));

if (document.fullscreenEnabled && document.documentElement.requestFullscreen) {
  $('btn-full').hidden = false;
  $('btn-full').addEventListener('click', async () => {
    try {
      if (document.fullscreenElement) await document.exitFullscreen();
      else await document.documentElement.requestFullscreen();
    } catch {
      $('stat').textContent = '当前浏览器无法进入全屏。';
    }
  });
  document.addEventListener('fullscreenchange', () => {
    $('btn-full').textContent = document.fullscreenElement ? '退出全屏' : '全屏';
  });
}

$('display').addEventListener('change', (ev) => {
  const id = Number(ev.target.value);
  if (Number.isFinite(id)) selectDisplay(id);
});

$('btn-quality').addEventListener('click', () => {
  const levels = [40, 55, 70, 85, 95];
  quality = levels[(levels.indexOf(quality) + 1) % levels.length];
  $('btn-quality').textContent = '画质 ' + quality;
  selectDisplay(currentDisplay);
});

// Retire the previous releases' storage and URL-based password flow.
try { sessionStorage.removeItem('vr.code'); } catch { /* Storage may be disabled. */ }
const url = new URL(location.href);
if (url.searchParams.has('code')) {
  url.searchParams.delete('code');
  history.replaceState(null, '', url);
}
showGate('', false);

setInterval(() => client?.sendControl({ t: 'ping', ts: Date.now() }), 3000);
