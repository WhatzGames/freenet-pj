// Drives the published freenet-pj web app over the Chrome DevTools Protocol.
//
// Why this exists rather than `chrome --screenshot` or a DevTools extension:
//
//   * The node serves a *shell* page that holds an EventSource open for the
//     lifetime of the tab. It therefore never goes quiescent, and Chrome's own
//     `--screenshot` flag waits for a load that never completes — it hangs until
//     killed. Driving CDP lets us decide when to shoot.
//   * That shell loads the app in a sandboxed iframe on an opaque origin, so the
//     top frame cannot reach into it. `Target.setAutoAttach` gives us a session
//     inside the app itself, which is what `--in-frame` uses.
//
// usage:
//   node .claude/skills/run-freenet-pj/driver.mjs <out.png> [options]
//
//   --hash '#<boardId>'     route to open (also '#org/<id>', '#me')
//   --w 1440 --h 900        viewport; < 600 turns on mobile emulation
//   --wait 9000             ms to let wasm boot and the node answer
//   --scheme dark|light     emulates prefers-color-scheme
//   --js '<expression>'     runs in the shell frame; async, may `return`
//   --in-frame              runs --js inside the app instead of the shell
//   --full                  capture beyond the viewport
//   --port 9333             debugging port; use distinct ports to run in parallel
//   --base <url>            defaults to $PJ_URL or the published address below
//
// Anything --js returns is printed to stdout. Return a JSON string if you want
// structured output; objects come back as-is via returnByValue.

import { writeFileSync } from "node:fs";
import { spawn } from "node:child_process";

const args = process.argv.slice(2);
const out = args[0];
const flag = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i === -1 ? fallback : args[i + 1];
};
const has = (name) => args.includes(`--${name}`);

if (!out || out.startsWith("--")) {
  console.error("usage: node driver.mjs <out.png> [--hash '#…'] [--js '…'] [--in-frame]");
  process.exit(2);
}

const W = Number(flag("w", 1440));
const H = Number(flag("h", 900));
const WAIT = Number(flag("wait", 9000));
const HASH = flag("hash", "");
const JS = flag("js", "");
const SCHEME = flag("scheme", "dark");
const PORT = Number(flag("port", 9333));

const BASE =
  flag("base", process.env.PJ_URL) ??
  "http://127.0.0.1:7509/v1/contract/web/6LpX8WFjTt2jad6TsvwM74XJ45W7oF3DFzF9JswKsTxS/";

const CHROME =
  process.env.CHROME_PATH ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

const chrome = spawn(
  CHROME,
  [
    "--headless=new",
    "--disable-gpu",
    "--hide-scrollbars",
    `--remote-debugging-port=${PORT}`,
    // Per-port profile so parallel runs do not fight over one profile lock.
    `--user-data-dir=/tmp/pj-driver-${PORT}`,
    `--window-size=${W},${H}`,
    "--no-first-run",
    "--no-default-browser-check",
    "about:blank",
  ],
  { stdio: "ignore" },
);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function endpoint() {
  for (let i = 0; i < 60; i++) {
    try {
      const r = await fetch(`http://127.0.0.1:${PORT}/json/version`);
      return (await r.json()).webSocketDebuggerUrl;
    } catch {
      await sleep(250);
    }
  }
  throw new Error("Chrome never opened its debugging port");
}

class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    // Sessions for the app's iframe, which is a target of its own because the
    // sandbox puts it on an opaque origin.
    this.frameSessions = [];
    ws.onmessage = (e) => {
      const msg = JSON.parse(e.data);
      if (msg.method === "Target.attachedToTarget") {
        this.frameSessions.push(msg.params.sessionId);
        return;
      }
      const p = this.pending.get(msg.id);
      if (p) {
        this.pending.delete(msg.id);
        msg.error ? p.reject(new Error(JSON.stringify(msg.error))) : p.resolve(msg.result);
      }
    };
  }
  send(method, params = {}, sessionId) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params, sessionId }));
    });
  }
}

// Node 22+ ships a global WebSocket, so CDP needs no dependency.
const open = (url) =>
  new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    ws.onopen = () => resolve(ws);
    ws.onerror = reject;
  });

try {
  const cdp = new Cdp(await open(await endpoint()));

  const { targetId } = await cdp.send("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await cdp.send("Target.attachToTarget", { targetId, flatten: true });
  const call = (m, p) => cdp.send(m, p, sessionId);

  await call("Page.enable");
  await call("Runtime.enable");
  await call("Target.setAutoAttach", {
    autoAttach: true,
    waitForDebuggerOnStart: false,
    flatten: true,
  });
  await call("Emulation.setEmulatedMedia", {
    features: [{ name: "prefers-color-scheme", value: SCHEME }],
  });
  await call("Emulation.setDeviceMetricsOverride", {
    width: W,
    height: H,
    deviceScaleFactor: 2,
    mobile: W < 600,
  });

  await call("Page.navigate", { url: BASE + HASH });
  await sleep(WAIT);

  if (JS && has("in-frame")) {
    let ran = false;
    for (const frame of cdp.frameSessions) {
      const probe = await cdp.send(
        "Runtime.evaluate",
        { expression: "!!document.querySelector('.app')", returnByValue: true },
        frame,
      );
      if (!probe.result?.value) continue;
      const r = await cdp.send(
        "Runtime.evaluate",
        { expression: `(async () => { ${JS} })()`, awaitPromise: true, returnByValue: true },
        frame,
      );
      if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails.exception));
      console.log(JSON.stringify(r.result?.value));
      ran = true;
      break;
    }
    if (!ran) throw new Error("no attached frame contained the app — raise --wait");
    await sleep(700);
  } else if (JS) {
    // In the shell frame. Helpers mirror what the app's own code does, so a
    // snippet written here reads much like one written inside the app.
    const wrapped = `(async () => {
      const frame = () => document.querySelector('iframe');
      for (let i = 0; i < 100; i++) {
        const f = frame();
        if (f && f.contentDocument && f.contentDocument.querySelector('.app')) break;
        await new Promise(r => setTimeout(r, 200));
      }
      const doc = () => document.querySelector('iframe').contentDocument;
      const win = () => document.querySelector('iframe').contentWindow;
      const all = (s) => Array.from(doc().querySelectorAll(s));
      const byText = (s, t) => all(s).find(e => (e.innerText||e.value||'').trim().toLowerCase().includes(t.toLowerCase()));
      const click = (el) => { if (!el) throw new Error('nothing to click'); el.dispatchEvent(new (win()).MouseEvent('click', {bubbles:true, cancelable:true})); };
      const clickText = (s, t) => click(byText(s, t));
      const fill = (el, v) => { el.value = v; el.dispatchEvent(new (win()).Event('input', {bubbles:true})); el.dispatchEvent(new (win()).Event('change', {bubbles:true})); };
      const wait = (ms) => new Promise(r => setTimeout(r, ms||400));
      return await (async () => { ${JS} })();
    })()`;
    const r = await call("Runtime.evaluate", {
      expression: wrapped,
      awaitPromise: true,
      returnByValue: true,
    });
    if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails.exception));
    if (r.result?.value !== undefined) console.log(JSON.stringify(r.result.value));
    await sleep(700);
  }

  const shot = await call("Page.captureScreenshot", {
    format: "png",
    captureBeyondViewport: has("full"),
  });
  writeFileSync(out, Buffer.from(shot.data, "base64"));
  console.log(`wrote ${out} (${W}x${H}, ${SCHEME})`);
} finally {
  chrome.kill();
}
