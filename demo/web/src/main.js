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

const DEFAULT_SECONDS = 15 * 60;

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
  return term;
}

function sendBinary(bytes) {
  if (sock && sock.readyState === WebSocket.OPEN && connected) sock.send(bytes);
}

function sendSize() {
  if (!sock || sock.readyState !== WebSocket.OPEN || !term) return;
  sock.send(JSON.stringify({ type: "resize", cols: term.cols || 80, rows: term.rows || 24 }));
}

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
      setTimeout(() => sendBinary(enc.encode(p)), 350);
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

for (const chip of document.querySelectorAll(".chip")) {
  chip.addEventListener("click", () => {
    const prompt = chip.dataset.prompt || "";
    if (connected) {
      sendBinary(enc.encode(prompt));
      if (term) term.focus();
      setStatus("Pasted into the prompt — press Enter to send.");
    } else {
      pendingPrompt = prompt;
      setStatus("Starting a session, then pasting the prompt…");
      startSession();
    }
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

window.addEventListener("resize", sendSize);

if (!("WebSocket" in window)) {
  setStatus("This browser has no WebSocket support, so the live demo can't start. The example session above is static.", "error");
}
