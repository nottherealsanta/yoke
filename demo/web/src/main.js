import { WTerm } from "@wterm/dom";
import { GhosttyCore } from "@wterm/ghostty";
import wasmUrl from "@wterm/ghostty/ghostty-vt.wasm?url";

const $ = (id) => document.getElementById(id);
const termEl = $("term");
const screen = $("term-screen");
const statusEl = $("status");
const startBtn = $("start");
const endBtn = $("end");
const sessionRow = $("session-row");
const timerEl = $("timer");
const connEl = $("conn");
const connLabel = $("conn-label");
const byoField = $("byo-field");
const keyInput = $("api-key");
const providedNote = $("provided-note");

const enc = new TextEncoder();
let term = null;
let core = null;
let sock = null;
let mode = "provided";
let connected = false;
let ended = false;
let connecting = false;
let pendingPrompt = "";
let endsAt = 0;
let ticker = null;
let charRatio = 0.6;
let fitTimer = null;

const DEFAULT_SECONDS = 15 * 60;
const KEY_SEQ = {
  esc: "\x1b",
  tab: "\t",
  up: "\x1b[A",
  down: "\x1b[B",
  left: "\x1b[D",
  right: "\x1b[C",
  enter: "\r",
  ctrlc: "\x03",
  mode1: "1",
  mode2: "2",
  mode3: "3",
};

/* ---------- small UI helpers ---------- */

function setStatus(msg, kind = "") {
  statusEl.textContent = msg;
  statusEl.className = "status" + (kind ? " " + kind : "");
}

function setConn(label, live = false) {
  connLabel.textContent = label;
  connEl.classList.toggle("live", live);
}

function setStartLabel(label) {
  startBtn.textContent = label;
}

function fmtClock(seconds) {
  const s = Math.max(0, Math.round(seconds));
  return `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
}

function fmtWait(seconds) {
  if (!seconds || seconds < 1) return "a little while";
  if (seconds < 90) return `${Math.round(seconds)} seconds`;
  const m = Math.round(seconds / 60);
  if (m < 90) return `${m} minutes`;
  return `${Math.round(m / 60)} hours`;
}

function startTicker(seconds) {
  stopTicker();
  endsAt = Date.now() + Math.max(1, seconds) * 1000;
  const tick = () => {
    const left = Math.max(0, (endsAt - Date.now()) / 1000);
    timerEl.textContent = fmtClock(left);
    timerEl.classList.toggle("low", left <= 60);
    if (left <= 0) stopTicker();
  };
  tick();
  ticker = setInterval(tick, 1000);
  sessionRow.hidden = false;
}

function stopTicker() {
  if (ticker) clearInterval(ticker);
  ticker = null;
}

function resetSessionUI(label = "Start a new session") {
  connected = false;
  stopTicker();
  sessionRow.hidden = true;
  endBtn.hidden = true;
  setConn("not started");
  setStartLabel(label);
  timerEl.textContent = fmtClock(DEFAULT_SECONDS);
  timerEl.classList.remove("low");
}

function isNarrow() {
  return window.matchMedia("(pointer: coarse)").matches || window.innerWidth <= 700;
}

function measureChar() {
  const ctx = document.createElement("canvas").getContext("2d");
  ctx.font = '13.5px "Monaspace Argon", monospace';
  const width = ctx.measureText("M").width;
  if (width > 0) charRatio = width / 13.5;
}

/* Size the grid to the frame: keep at least 60 columns on phones, 14-30 rows. */
function fitTerminal() {
  const wterm = termEl.classList.contains("wterm") ? termEl : termEl.querySelector(".wterm");
  const style = wterm ? getComputedStyle(wterm) : null;
  const padX = style ? parseFloat(style.paddingLeft) + parseFloat(style.paddingRight) : 24;
  const padY = style ? parseFloat(style.paddingTop) + parseFloat(style.paddingBottom) : 20;
  if (!screen.clientWidth) return;

  const narrow = isNarrow();
  const avail = Math.max(180, screen.clientWidth - padX);
  const base = 13.5;
  const minCols = 60;
  const landscapePhone = narrow && window.innerWidth > window.innerHeight;
  let font = landscapePhone ? 11 : base;
  if (avail / (base * charRatio) < minCols) font = avail / minCols / charRatio;
  font = Math.max(7.5, Math.min(base, Math.floor(font * 4) / 4));

  const rowHeight = Math.max(12, Math.round(font * 1.35));
  const viewportHeight = window.innerHeight;
  const budget = landscapePhone
    ? Math.min(viewportHeight * 0.78, 420)
    : narrow
      ? Math.min(viewportHeight * 0.58, 560)
      : Math.min(viewportHeight * 0.52, 520);
  const rows = Math.max(14, Math.min(30, Math.floor((budget - padY) / rowHeight)));
  const height = rows * rowHeight + padY;

  if (wterm) {
    wterm.style.setProperty("--term-font-size", `${font}px`);
    wterm.style.setProperty("--term-row-height", `${rowHeight}px`);
  }
  if (Math.abs(screen.clientHeight - height) > 1) screen.style.height = `${height}px`;

  if (term) {
    term.fit();
    sendSize();
  }
}

function scheduleFit() {
  clearTimeout(fitTimer);
  fitTimer = setTimeout(fitTerminal, 120);
}

/* ---------- key choice ---------- */

function choose(next) {
  mode = next;
  for (const b of document.querySelectorAll(".choice")) {
    b.setAttribute("aria-pressed", String(b.dataset.mode === next));
  }
  byoField.hidden = next !== "byo";
  providedNote.hidden = next === "byo";
  if (next === "byo") keyInput.focus();
}

for (const b of document.querySelectorAll(".choice")) {
  b.addEventListener("click", () => choose(b.dataset.mode));
}

/* ---------- terminal (created lazily; no connection on page load) ---------- */

async function ensureTerm() {
  if (term) return term;
  screen.classList.add("live");
  await Promise.all([
    document.fonts.load('13.5px "Monaspace Argon"'),
    document.fonts.load('italic 13.5px "Monaspace Argon"'),
    document.fonts.load('700 13.5px "Monaspace Argon"'),
  ]).catch(() => {});
  measureChar();
  core = await GhosttyCore.load({ wasmPath: wasmUrl, scrollbackLines: 5000 });
  term = new WTerm(termEl, {
    core,
    cursorBlink: true,
    maxImageWidth: 1200,
    maxImageHeight: 800,
    onData: (s) => sendBinary(enc.encode(s)),
    onBinary: (b) => sendBinary(b),
    onResize: sendSize,
    onClipboardWrite: (text) => navigator.clipboard?.writeText(text).catch(() => {}),
    onTitle: () => {},
  });
  await term.init();
  fitTerminal();
  return term;
}

function sendBinary(bytes) {
  if (sock && sock.readyState === WebSocket.OPEN && connected) sock.send(bytes);
}

function sendSize(cols, rows) {
  if (!sock || sock.readyState !== WebSocket.OPEN || !term) return;
  sock.send(JSON.stringify({
    type: "resize",
    cols: cols || term.cols || 80,
    rows: rows || term.rows || 24,
  }));
}

// Keep the soft keyboard available when a tap lands on the terminal grid.
termEl.addEventListener("click", () => {
  if (connected) term?.focus();
});

// Shift+Enter as a newline, for terminals without the Kitty protocol.
termEl.addEventListener("keydown", (e) => {
  if (e.key !== "Enter" || !e.shiftKey || e.ctrlKey || e.altKey || e.metaKey || e.isComposing) return;
  if ((term?.bridge?.kittyKeyboardFlags?.() ?? 0) !== 0) return;
  e.preventDefault();
  e.stopImmediatePropagation();
  sendBinary(enc.encode("\x1b\r"));
}, true);

/* ---------- session ---------- */

function closeSocket() {
  if (sock) {
    const s = sock;
    sock = null;
    try { s.close(1000, "restart"); } catch { /* already closed */ }
  }
}

async function startSession() {
  if (connecting) return;
  if (mode === "byo" && !keyInput.value.trim()) {
    setStatus("Paste your Groq key, or switch to the free demo tokens.", "error");
    keyInput.focus();
    return;
  }

  closeSocket();
  connecting = true;
  ended = false;
  connected = false;
  startBtn.disabled = true;
  setStatus("Preparing a scratch folder and starting yoke…");
  setConn("connecting");
  setStartLabel("Starting…");
  endBtn.hidden = true;
  sessionRow.hidden = true;

  try {
    await ensureTerm();
    fitTerminal();
  } catch (err) {
    console.error("terminal failed to start", err);
    connecting = false;
    startBtn.disabled = false;
    screen.classList.remove("live");
    resetSessionUI();
    setStatus("This browser could not start the terminal. The static example above shows what a session looks like.", "error");
    return;
  }

  if (term) term.write("\x1bc");

  const proto = location.protocol === "https:" ? "wss" : "ws";
  const s = new WebSocket(`${proto}://${location.host}/ws`);
  s.binaryType = "arraybuffer";
  sock = s;

  s.onopen = () => {
    const payload = {
      type: "start",
      cols: term?.cols || 80,
      rows: term?.rows || 24,
      key: mode === "byo" ? keyInput.value.trim() : "",
    };
    s.send(JSON.stringify(payload));
  };

  s.onmessage = (e) => {
    if (typeof e.data === "string") {
      handleServer(e.data);
      return;
    }
    if (term) term.write(new Uint8Array(e.data));
  };

  s.onerror = () => {
    if (sock !== s) return;
    connecting = false;
    startBtn.disabled = false;
    setStatus("Could not reach the demo server. The static example above is still here to read.", "error");
    setConn("offline");
    setStartLabel("Try again");
  };

  s.onclose = () => {
    if (sock !== s) return;
    sock = null;
    connecting = false;
    startBtn.disabled = false;
    if (!ended) {
      ended = true;
      stopTicker();
      setStatus("The session closed. Start another one whenever you like.", "error");
      setConn("closed");
      setStartLabel("Start a new session");
      endBtn.hidden = true;
    }
  };
}

function handleServer(raw) {
  let m;
  try { m = JSON.parse(raw); } catch { return; }

  if (m.type === "ready") {
    connecting = false;
    connected = true;
    startBtn.disabled = false;
    startBtn.textContent = "Restart session";
    endBtn.hidden = false;
    setConn("live", true);
    setStatus("Live. Only read, write and edit are enabled; the folder disappears when you leave.", "good");
    startTicker(m.limit_seconds || DEFAULT_SECONDS);
    if (term) term.focus();
    if (pendingPrompt) {
      const p = pendingPrompt;
      pendingPrompt = "";
      setTimeout(() => {
        if (sendPrompt(p)) setStatus("Prompt sent. Watch the context fill up.", "good");
      }, 350);
    }
    return;
  }

  if (m.type === "limit") {
    ended = true;
    connecting = false;
    connected = false;
    startBtn.disabled = false;
    const wait = m.retry_after ? ` Try again in ${fmtWait(m.retry_after)}.` : "";
    setStatus((m.msg || "The free demo tokens are used up for now.") + wait, "error");
    setConn("limited");
    resetSessionUI("Start a new session");
    return;
  }

  if (m.type === "end") {
    ended = true;
    connecting = false;
    connected = false;
    startBtn.disabled = false;
    stopTicker();
    setStatus(m.reason || "This session ended.", "");
    setConn("ended");
    setStartLabel("Start a new session");
    endBtn.hidden = true;
    sessionRow.hidden = true;
    return;
  }

  if (m.type === "status") {
    if (!connected) {
      ended = true;
      connecting = false;
      startBtn.disabled = false;
      setStartLabel("Start a new session");
      setConn("unavailable");
    }
    setStatus(m.msg || "", "error");
  }
}

function endSession() {
  ended = true;
  closeSocket();
  resetSessionUI("Start a new session");
  setStatus("Session ended. The scratch folder is gone.");
}

/* ---------- controls ---------- */

startBtn.addEventListener("click", startSession);
endBtn.addEventListener("click", endSession);

function sendPrompt(prompt) {
  if (!connected || !prompt) return false;
  sendBinary(enc.encode(prompt));
  // Let the composer paint before Enter, so the whole prompt is submitted.
  setTimeout(() => {
    if (connected) sendBinary(enc.encode("\r"));
  }, 140);
  if (term) term.focus();
  return true;
}

for (const chip of document.querySelectorAll(".chip")) {
  chip.addEventListener("click", () => {
    const prompt = chip.dataset.prompt || "";
    if (connected) {
      if (sendPrompt(prompt)) setStatus("Sent. The agent is working on it.", "good");
    } else {
      pendingPrompt = prompt;
      setStatus("Starting a session, then sending the prompt…");
      startSession();
    }
  });
}

for (const key of document.querySelectorAll(".key")) {
  const press = () => {
    if (!connected) {
      setStatus("Start a session first; then these keys drive the TUI.", "");
      return;
    }
    const seq = KEY_SEQ[key.dataset.key];
    if (!seq) return;
    sendBinary(enc.encode(seq));
    if (term) term.focus();
  };
  // Keep focus (and the soft keyboard) on the terminal.
  key.addEventListener("pointerdown", (e) => {
    e.preventDefault();
    press();
  });
  key.addEventListener("click", (e) => {
    if (e.detail === 0) press();
  });
}

keyInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    startSession();
  }
});

/* ---------- copy buttons ---------- */

for (const [id, source] of [["copy-embed", "embed-code"], ["copy-install", "install-cmd"]]) {
  const btn = $(id);
  const el = $(source);
  if (!btn || !el) continue;
  btn.addEventListener("click", async () => {
    const text = el.textContent.trim();
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const ta = document.createElement("textarea");
      ta.value = text;
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      ta.remove();
    }
    const old = btn.textContent;
    btn.textContent = "copied";
    setTimeout(() => { btn.textContent = old; }, 1200);
  });
}

window.addEventListener("resize", scheduleFit);
window.addEventListener("orientationchange", () => setTimeout(fitTerminal, 250));
fitTerminal();
document.fonts?.ready.then(() => {
  measureChar();
  fitTerminal();
});

if (!("WebSocket" in window)) {
  setStatus("This browser has no WebSocket support, so the live demo can't start. The example session above is static.", "error");
}
