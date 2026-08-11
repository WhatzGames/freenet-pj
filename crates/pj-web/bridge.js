// Bridge to the Freenet shell.
//
// A node does not serve a web contract's page directly. It serves a shell page
// that loads the app in an iframe with `sandbox="allow-scripts …"` — no
// `allow-same-origin` — so the app runs on an opaque origin and cannot open a
// socket to the node itself. Instead the shell proxies the node's client API over
// postMessage, injecting an auth token the app never sees. That is deliberate:
// the app is untrusted code fetched from a P2P network, so it gets no ambient
// authority over the node.
//
// This file adapts that proxy back into something with a `WebSocket` shape, which
// is what `freenet_stdlib::client_api::WebApi` expects. Only the members WebApi
// actually touches are implemented; the Rust side hands the result to
// `unchecked_into::<WebSocket>()`.
//
// Protocol (shell ⇄ app), as implemented by the node's shell script:
//   app → shell  { __freenet_ws__: true, type: 'open',  id, url }
//                { __freenet_ws__: true, type: 'send',  id, data: ArrayBuffer }
//                { __freenet_ws__: true, type: 'close', id, code, reason }
//   shell → app  { __freenet_ws__: true, type: 'open'|'message'|'close'|'error', id, … }
//   app → shell  { __freenet_shell__: true, type: 'hash', hash: '#…' }

(function () {
  "use strict";

  var nextId = 1;
  var sockets = new Map();
  // The shell is the parent frame. Outside a frame there is no shell, which is
  // the local-development case, and a plain WebSocket is used instead.
  var framed = window.self !== window.top;

  if (framed) {
    window.addEventListener("message", function (event) {
      var msg = event.data;
      if (!msg || msg.__freenet_ws__ !== true) return;

      var socket = sockets.get(msg.id);
      if (!socket) return;

      switch (msg.type) {
        case "open":
          socket.readyState = 1; // OPEN
          if (socket.onopen) socket.onopen({ type: "open" });
          break;

        case "message":
          // WebApi expects an ArrayBuffer, matching binaryType = 'arraybuffer'.
          if (socket.onmessage) socket.onmessage({ data: msg.data });
          break;

        case "close":
          socket.readyState = 3; // CLOSED
          sockets.delete(msg.id);
          if (socket.onclose)
            socket.onclose({
              code: msg.code,
              reason: msg.reason || "",
              wasClean: true,
            });
          break;

        case "error":
          // WebApi's error handler reads these three fields off the event.
          if (socket.onerror)
            socket.onerror({
              message: "the Freenet shell refused or dropped the connection",
              filename: "bridge.js",
              lineno: 0,
            });
          break;
      }
    });
  }

  function ShellSocket(url) {
    this.url = url;
    this.readyState = 0; // CONNECTING
    this.binaryType = "arraybuffer";
    this.onopen = null;
    this.onmessage = null;
    this.onerror = null;
    this.onclose = null;

    this._id = nextId++;
    sockets.set(this._id, this);
    parent.postMessage(
      { __freenet_ws__: true, type: "open", id: this._id, url: url },
      "*",
    );
  }

  ShellSocket.prototype.send = function (data) {
    // `data` is a view straight into the wasm heap. Structured-cloning a view
    // clones its whole backing buffer — the entire linear memory — so copy the
    // bytes out first and transfer only that copy.
    var copy = new Uint8Array(data);
    parent.postMessage(
      { __freenet_ws__: true, type: "send", id: this._id, data: copy.buffer },
      "*",
      [copy.buffer],
    );
  };

  ShellSocket.prototype.close = function (code, reason) {
    this.readyState = 3;
    sockets.delete(this._id);
    parent.postMessage(
      {
        __freenet_ws__: true,
        type: "close",
        id: this._id,
        code: code,
        reason: reason,
      },
      "*",
    );
  };

  /// Opens a connection to the node: through the shell when framed, directly
  /// otherwise.
  window.__freenetSocket = function (url) {
    return framed ? new ShellSocket(url) : new WebSocket(url);
  };

  /// Wall clock as a BigInt of milliseconds since the epoch.
  ///
  /// Temporal, not `Date.now()`, because `Date.now()` is a Number: reading it from
  /// Rust means an `f64 -> u64` conversion, and Rust has no fallible one — every
  /// such call site is an unchecked `as` with silent saturation. `epochNanoseconds`
  /// is a BigInt, so the whole path from clock to op stays integral and the
  /// conversion on the Rust side can fail loudly instead.
  ///
  /// BigInt division truncates toward zero, which is what we want: milliseconds,
  /// not milliseconds-rounded-up.
  ///
  /// The fallback is for a browser without Temporal, and converts inside JS so the
  /// Rust side only ever sees a BigInt either way.
  window.__freenetNowMs = function () {
    if (typeof Temporal !== "undefined") {
      return Temporal.Now.instant().epochNanoseconds / 1000000n;
    }
    return BigInt(Date.now());
  };

  /// Mirrors the app's route onto the shell's address bar so the URL in the
  /// browser is the one a user can copy and share. Inside the frame our own
  /// `location.hash` is invisible to them.
  window.__freenetSetHash = function (hash) {
    if (!framed || !hash) return;
    parent.postMessage(
      { __freenet_shell__: true, type: "hash", hash: hash },
      "*",
    );
  };

  /// Closes every proxied socket, as the node would if it went away.
  ///
  /// A diagnostic, not a feature: connection recovery is the kind of code that is
  /// either exercised or merely hoped for, and there is otherwise no way to make the
  /// socket drop on demand. Returns how many were closed.
  window.__freenetDropSocket = function () {
    var closed = 0;
    sockets.forEach(function (socket) {
      if (socket.readyState === 1 || socket.readyState === 0) {
        closed += 1;
        parent.postMessage(
          { __freenet_ws__: true, type: "close", id: socket._id },
          "*",
        );
        socket.readyState = 3;
        if (socket.onclose) {
          socket.onclose({ code: 1006, reason: "dropped for testing", wasClean: false });
        }
      }
    });
    sockets.clear();
    return closed;
  };

  /// Offers a URL through the platform's own share sheet, falling back to the
  /// clipboard. On a phone this is the difference between tapping "share" and
  /// transcribing 44 characters of base58 by hand.
  ///
  /// Returns a word describing what happened so the UI can say something true.
  /// Puts a link where the user can use it.
  ///
  /// The clipboard is tried *first*, and that ordering is the whole point. This
  /// used to prefer `navigator.share`, which looks like the friendlier option and
  /// is not: inside a sandboxed frame the share sheet ignores the `url` we hand it
  /// and offers the top-level page address instead. So "Copy link" on a task copied
  /// the app's own URL with no fragment — the one thing it must never do, because
  /// the fragment *is* the link.
  ///
  /// `navigator.share` stays as the fallback for a browser with no clipboard API,
  /// where sharing the page is still better than nothing.
  window.__freenetShare = function (title, url) {
    if (navigator.clipboard) {
      navigator.clipboard.writeText(url).catch(function () {});
      return "copied";
    }
    if (navigator.share) {
      // Fire and forget: a rejection here is the user dismissing the sheet.
      navigator.share({ title: title, url: url }).catch(function () {});
      return "shared";
    }
    return "unavailable";
  };

  window.__freenetSetTitle = function (title) {
    if (!framed || !title) return;
    parent.postMessage(
      { __freenet_shell__: true, type: "title", title: title },
      "*",
    );
  };
})();
