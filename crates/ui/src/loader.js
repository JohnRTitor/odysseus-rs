/* The boot loader's behaviour — ported from `static/index.html:259-293`.
 *
 * Two constraints shape this file, and both were learned the hard way.
 *
 * 1. **No inline style attribute anywhere, including inside `innerHTML` strings.**
 *    The shell is served under `style-src 'self'` (T0112), and CSP *does* check
 *    style attributes in markup parsed via `innerHTML` — not just in the server's
 *    HTML. The Python original interpolated one straight into a string -- a `<span>`
 *    carrying position, left, top and a `translate(-50%, <y>px)` transform:
 *
 *        el.innerHTML = '<span ' + ATTR + '>' + dot + '</span><span>' + wave + '</span>'
 *
 *    Written out rather than quoted, because quoting it verbatim would put the
 *    offending attribute into this comment -- and this comment is served to the
 *    browser, where the test that forbids the attribute would then fail on it.
 *    which the tightened policy refuses, leaving an unstyled dot and no error. The
 *    moving part is a class plus a CSS custom property written through the CSSOM
 *    (`setProperty`), which CSP does not gate.
 *
 *    Note this comment is itself part of the served document, because the script
 *    is inlined. That is why the offending construct above is written across two
 *    lines: a test asserts the served page contains no style attribute at all, and
 *    a comment that quoted one verbatim would fail it. The same test guards this
 *    file, so the constraint and its enforcement cannot drift apart.
 *
 * 2. **This script must run even if hydration never does.** That is the entire
 *    point of the 5 s fallback below: it exists for a boot that never reaches the
 *    app bundle. So it is an inline nonce-gated script in the document rather than
 *    part of the wasm hydrate path, which is exactly where the Python has it too.
 *
 * The wave frames, the 150 ms interval and the 5 s/300 ms timings are ported
 * exactly. The frames are data, not decoration: `render()` must produce
 * byte-identical output at a given `i`, or the animation's rhythm changes.
 */

/* The nine frames, in order, exactly as `static/index.html:261-271`. */
var FRAMES = [
  { wave: '▁▂▃', y: 9 },
  { wave: '▂▃▄', y: 7 },
  { wave: '▃▄▅', y: 5 },
  { wave: '▄▅▆', y: 3 },
  { wave: '▅▆▅', y: 1 },
  { wave: '▆▅▄', y: 3 },
  { wave: '▅▄▃', y: 5 },
  { wave: '▄▃▂', y: 7 },
  { wave: '▃▂▁', y: 9 }
];

var FRAME_MS = 150;
/* Last-resort fallback. Must still REMOVE the node, not merely fade it: the app
 * reads the loader's presence as "startup in progress" and holds off clearing the
 * composer while it exists. A fade that leaves the node would wedge that forever. */
var FALLBACK_MS = 5000;
/* Matches the overlay's `transition-opacity duration-300`; the node is removed only
 * once the fade has actually run, otherwise it pops rather than fades. */
var FADE_MS = 300;

var wave = document.getElementById('loader-wave');
var overlay = document.getElementById('app-loader');

function stop() {
  if (interval !== null) {
    clearInterval(interval);
    interval = null;
  }
}

var interval = null;

function render() {
  if (!wave) {
    return;
  }
  var frame = FRAMES[i % FRAMES.length];
  i++;
  wave.innerHTML =
    '<span class="absolute left-1/2 top-[var(--wave-y)] -translate-x-1/2 text-[1.24em] leading-none">•</span>' +
    '<span>' + frame.wave + '</span>';
  // CSSOM, so no CSP exception is needed for a value that changes every frame.
  var dot = wave.firstElementChild;
  if (dot) {
    dot.style.setProperty('--wave-y', frame.y + 'px');
  }
}

var i = 0;

/* Hidden by the app once the shell is wired. Exposed on `window` because the
 * interval is owned by this script, so the caller has no other handle on it --
 * the Python does the same with `__odysseusLoaderWaveStop`. */
window.__odysseusLoaderWaveStop = stop;

if (overlay) {
  render();
  interval = setInterval(function () {
    // The app removed the overlay, so there is nothing left to animate.
    if (!document.getElementById('app-loader')) {
      stop();
      return;
    }
    render();
  }, FRAME_MS);

  setTimeout(function () {
    var node = document.getElementById('app-loader');
    if (!node) {
      return;
    }
    stop();
    node.style.opacity = '0';
    setTimeout(function () {
      node.remove();
    }, FADE_MS);
  }, FALLBACK_MS);
}