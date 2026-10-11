// Client + build-time logic for Terminal.astro (asciinema cast player).

// An open player is created on first hover/focus/touch.
const INTERACTION = ['pointerenter', 'focusin', 'touchstart'];

/** Options for `AsciinemaPlayer.create`, serialised as `data-options`; `autoPlay` is false whenever `src` is set (DOC-EX-15). @typedef {{ cols?: number, rows?: number, autoPlay: boolean, speed: number, idleTimeLimit: number, loop: boolean, fit: 'width' | 'height' | 'both' | false }} PlayerOptions */

/** The subset of the asciinema-player instance Terminal drives. @typedef {{ play: () => unknown, pause: () => unknown, seek: (pos: number) => unknown, getCurrentTime: () => Promise<number>, getDuration: () => Promise<number | undefined>, addEventListener: (name: string, cb: (data?: unknown) => void) => void, dispose: () => void }} Player */

/** The asciinema-player module shape (`create` only). @typedef {{ create: (src: { data: string }, el: HTMLElement, opts: Record<string, unknown>) => Player }} PlayerModule */

/**
 * Prefix a base-relative, root-absolute `path` with the site base; relative paths and full URLs pass through.
 * @param {string} path
 * @param {string} base
 * @returns {string}
 */
export function withBase(path, base) {
  if (!path.startsWith('/') || path.startsWith('//')) return path;
  return (base.endsWith('/') ? base : `${base}/`) + path.slice(1);
}

let seq = 0;
/** Deterministic per-build element id (never random: reproducible output). */
export const nextTerminalId = () => `ocx-terminal-${++seq}`;

/**
 * Player JS + its CSS (vendor sheet in the `ocx.vendor` cascade layer), fetched only when a player is created.
 * The sheet goes in by `?url` + a `<link>`: a plain dynamic CSS import gets hoisted into the page's
 * render-blocking `<link>`s by Astro's CSS graph.
 * Memoized per page: a second terminal expanded before the sheet loads must wait for the same `<link>`.
 * @returns {Promise<PlayerModule>}
 */
async function loadPlayerAndSheet() {
  /** @type {unknown[]} */
  const mods = await Promise.all([
    // @ts-expect-error asciinema-player ships no types; cast to PlayerModule below
    import('asciinema-player'),
    import('./terminal-player.css?url'),
  ]);
  const href = /** @type {{ default: string }} */ (mods[1]).default;
  if (!document.querySelector(`link[href="${href}"]`)) {
    const link = Object.assign(document.createElement('link'), { rel: 'stylesheet', href });
    await new Promise((resolve) => {
      link.addEventListener('load', resolve);
      link.addEventListener('error', resolve); // an unstyled player beats none
      document.head.append(link);
    });
  }
  return /** @type {PlayerModule} */ (mods[0]);
}
/** @type {Promise<PlayerModule> | undefined} */
let loading;
export const loadDefault = () => (loading ??= loadPlayerAndSheet());

const reducedMotion = () => matchMedia('(prefers-reduced-motion: reduce)').matches;

/** @param {number} s @returns {string} `m:ss`, never negative. */
const formatTime = (s) => {
  const t = Math.max(0, Math.round(s || 0));
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, '0')}`;
};

/**
 * Wire the custom control bar (rendered with `hidden` in Terminal.astro) to a created player:
 * play/pause toggle, a native `<input type=range>` seek bar (native = free keyboard support:
 * arrow keys already seek it), a time readout, and fullscreen on the `.ocx-terminal` root.
 * Space/other transport shortcuts on the player itself still work (asciinema-player's own keydown
 * handler is unconditional; only its visible bar is gated by `controls: false`) whenever it has
 * focus, on top of what our own controls give for free.
 * @param {HTMLElement} root
 * @param {Player} player
 * @returns {void}
 */
function wireControls(root, player) {
  const controls = /** @type {HTMLElement | null} */ (root.querySelector('.ocx-terminal__controls'));
  if (!controls) return;
  const play = /** @type {HTMLButtonElement} */ (controls.querySelector('.ocx-terminal__play'));
  const seek = /** @type {HTMLInputElement} */ (controls.querySelector('.ocx-terminal__seek'));
  const current = /** @type {HTMLElement} */ (controls.querySelector('.ocx-terminal__current'));
  const durationEl = /** @type {HTMLElement} */ (controls.querySelector('.ocx-terminal__duration'));
  const fullscreen = /** @type {HTMLButtonElement | null} */ (controls.querySelector('.ocx-terminal__fullscreen'));

  let duration = 0;
  let dragging = false;
  // Live-scrub state (see scrubTo): the thumb leads the player until the last seek settles.
  /** @type {number | undefined} */
  let pending;
  let inFlight = false;
  /** @type {number | undefined} */
  let raf;

  // Called every frame; write only on change, or style/layout is invalidated 60x/s
  // and a screen reader may re-announce the slider.
  /** @param {number} t */
  const showTime = (t) => {
    const s = formatTime(t);
    if (current.textContent !== s) current.textContent = s;
    const v = `${s} of ${formatTime(duration)}`;
    if (duration > 0 && seek.getAttribute('aria-valuetext') !== v) seek.setAttribute('aria-valuetext', v);
    if (duration > 0) seek.style.setProperty('--_p', String(Math.min(1, t / duration))); // played fill
  };
  const snapToEnd = () => {
    seek.value = '1000';
    showTime(duration);
  };
  /** @param {number} t */
  const sync = (t) => {
    if (duration > 0 && t >= duration) {
      snapToEnd();
      return;
    }
    if (duration > 0) seek.value = String((t / duration) * 1000); // step="any": no rounding, sub-second smooth
    showTime(t);
  };
  // requestAnimationFrame, not a fixed-interval timer: sub-second, and paused for free whenever the
  // tab is backgrounded. Reschedules itself first so a `dragging` bail-out never stops the loop.
  // Only 'pause'/'ended' stop it: a `loop` cast restarts at the end without any event, so a frame
  // at t >= duration must not stop it (sync just snaps to the end).
  const tick = () => {
    raf = requestAnimationFrame(tick);
    if (dragging || inFlight) return;
    void player.getCurrentTime().then(sync);
  };
  const stopLoop = () => {
    if (raf !== undefined) cancelAnimationFrame(raf);
    raf = undefined;
  };
  /** @param {boolean} playing */
  const setPlaying = (playing) => {
    controls.dataset['state'] = playing ? 'playing' : 'paused';
    play.setAttribute('aria-label', playing ? 'Pause' : 'Play');
    if (playing) raf ??= requestAnimationFrame(tick);
    else stopLoop();
  };

  player.addEventListener('metadata', (data) => {
    duration = /** @type {{ duration?: number }} */ (data ?? {}).duration ?? 0;
    durationEl.textContent = formatTime(duration);
    seek.disabled = duration <= 0;
  });
  player.addEventListener('play', () => setPlaying(true));
  player.addEventListener('playing', () => setPlaying(true));
  player.addEventListener('pause', () => setPlaying(false));
  player.addEventListener('ended', () => {
    setPlaying(false);
    controls.dataset['state'] = 'ended';
    if (duration > 0) snapToEnd();
  });
  player.addEventListener('seeked', () => {
    // mid-drag and while a scrub seek is in flight the thumb leads, not the player (a stale
    // position would snap it back before the final seek lands)
    if (!dragging && !inFlight) void player.getCurrentTime().then(sync);
  });

  play.addEventListener('click', () => {
    void (controls.dataset['state'] === 'playing' ? player.pause() : player.play());
  });

  seek.addEventListener('pointerdown', (e) => {
    dragging = true;
    // Keep the drag on the slider when the pointer leaves it (or crosses the player).
    try {
      seek.setPointerCapture(e.pointerId);
    } catch {
      // synthetic/inactive pointer: native range drag still works
    }
  });
  // Live scrub: the terminal follows the thumb while dragging. One seek in flight at a time and
  // the latest position wins, so a fast drag never queues a backlog of re-renders.
  /** @param {number} t */
  const scrubTo = (t) => {
    pending = t;
    if (inFlight) return;
    inFlight = true;
    const next = () => {
      const target = /** @type {number} */ (pending);
      pending = undefined;
      void Promise.resolve(player.seek(target)).finally(() => {
        if (pending !== undefined) return next();
        inFlight = false;
        if (!dragging) void player.getCurrentTime().then(sync); // settle on where the player really landed
      });
    };
    next();
  };
  seek.addEventListener('input', () => {
    if (duration <= 0) return;
    const t = (Number(seek.value) / 1000) * duration;
    showTime(t);
    scrubTo(t);
  });
  seek.addEventListener('change', () => {
    dragging = false;
    if (duration > 0) scrubTo((Number(seek.value) / 1000) * duration);
  });
  // A click without a drag (or a cancelled gesture) never fires `change`: clear here too,
  // or `tick` stays frozen believing the user is still seeking.
  seek.addEventListener('pointerup', () => {
    dragging = false;
  });
  seek.addEventListener('pointercancel', () => {
    dragging = false;
  });

  if (fullscreen && !document.fullscreenEnabled) fullscreen.hidden = true;
  fullscreen?.addEventListener('click', () => {
    if (document.fullscreenElement === root) void document.exitFullscreen();
    else void root.requestFullscreen?.();
  });
  document.addEventListener('fullscreenchange', () => {
    controls.toggleAttribute('data-fullscreen', document.fullscreenElement === root);
    // asciinema-player switches to fit-both (whole terminal on screen) only on a `fullscreenchange`
    // at its own wrapper, and we fullscreen our root instead; forward it. Non-bubbling: never
    // re-enters this listener. ponytail: relies on 3.15.1's wrapper listener; re-check on upgrade.
    root.querySelector('.ap-wrapper')?.dispatchEvent(new Event('fullscreenchange'));
  });

  controls.hidden = false;
  // Only now may the control glyphs crossfade (Terminal.astro): never at init.
  root.toggleAttribute('data-live', true);
}

/**
 * Wire one rendered `.ocx-terminal` root. The cast is fetched lazily: an open terminal fetches when it nears
 * the viewport, a collapsed one when its chrome opens (`ocx:collapsible:change`, dispatched here on the root).
 * The player itself is created lazily too: an open terminal on its first hover, focus, touch or start click
 * (as it nears the viewport only with `autoPlay`); a collapsed one on its first expand.
 * @param {HTMLElement} root
 * @param {() => Promise<PlayerModule>} [loadPlayer] dynamic import of the player JS + CSS
 * @returns {void}
 */
export function mountTerminal(root, loadPlayer = loadDefault) {
  const area = /** @type {HTMLElement} */ (root.querySelector('.ocx-terminal__player'));
  /** @type {unknown} */
  const parsed = JSON.parse(root.dataset['options'] ?? '{}');
  const opts = /** @type {PlayerOptions} */ (parsed);
  const collapsed = root.dataset['collapsed'] !== undefined;
  /** @type {Promise<string> | undefined} */
  let cast;
  // Low priority: many casts on one page must not compete with the main content.
  const fetchCast = () => {
    if (!cast) {
      cast = fetch(root.dataset['src'] ?? '', { priority: 'low' }).then((r) => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        return r.text();
      });
      cast.catch(() => {}); // surfaced in the player area once a player is wanted
    }
    return cast;
  };

  // Our start overlay (SSR'd; asciinema's own is hidden in CSS): hidden once playback starts, and
  // a click starts it through the same create path as everything else.
  const start = /** @type {HTMLElement | null} */ (root.querySelector('.ocx-terminal__start'));
  /** @type {Promise<Player | undefined> | undefined} */
  let player;
  /** @param {boolean} autoPlay */
  const create = (autoPlay) =>
    // Wait for the web fonts too: the player measures its cell size from the mono font once, at create.
    (player ??= Promise.all([loadPlayer(), fetchCast(), document.fonts?.ready])
      .then(([m, d]) => {
        const p = m.create({ data: d }, area, {
          ...opts,
          autoPlay,
          preload: true, // the data is already here: show 00:00, not --:--, before play
          theme: 'ocx',
          terminalFontFamily: 'var(--ocx-font-mono)',
          controls: false, // our own bar (below) drives play/pause/seek/fullscreen through the player API
        });
        // The player inserts its own element into `area` next to our static controls; `append()`
        // on an already-present node just moves it, so this pins the bar at the bottom regardless
        // of which side asciinema-player inserts on (owner regression: it landed on top, 993b460).
        const controls = root.querySelector('.ocx-terminal__controls');
        if (controls) area.append(controls);
        wireControls(root, p);
        for (const name of ['play', 'playing']) p.addEventListener(name, () => start && (start.hidden = true));
        return p;
      })
      .catch(() => {
        area.textContent = 'The recording could not be loaded.';
        return undefined;
      }));

  start?.addEventListener('click', () => {
    start.hidden = true;
    void create(false).then((p) => p?.play());
  });

  if (!collapsed && !opts.autoPlay) {
    // The cast is fetched near the viewport; the player waits for interaction.
    const io = new IntersectionObserver(
      (entries) => {
        if (!entries.some((e) => e.isIntersecting)) return;
        io.disconnect();
        void fetchCast();
      },
      { rootMargin: '200px' },
    );
    io.observe(root);
    const arm = () => {
      for (const type of INTERACTION) root.removeEventListener(type, arm);
      io.disconnect();
      void create(false);
    };
    for (const type of INTERACTION) root.addEventListener(type, arm, { passive: true });
    return;
  }
  if (!collapsed) {
    const io = new IntersectionObserver(
      (entries) => {
        if (!entries.some((e) => e.isIntersecting)) return;
        io.disconnect();
        void create(opts.autoPlay && !reducedMotion());
      },
      { rootMargin: '200px' },
    );
    io.observe(root);
    return;
  }

  const trigger = /** @type {HTMLElement} */ (root.querySelector('.ocx-terminal__chrome'));
  trigger.addEventListener('click', () => {
    const expand = trigger.getAttribute('aria-expanded') !== 'true';
    trigger.setAttribute('aria-expanded', String(expand));
    area.hidden = !expand;
    root.dispatchEvent(new CustomEvent('ocx:collapsible:change', { detail: { open: expand }, bubbles: true }));
  });
  let open = false;
  root.addEventListener('ocx:collapsible:change', (event) => {
    if (event.target !== root) return;
    open = /** @type {CustomEvent<{ open: boolean }>} */ (event).detail.open;
    if (!open) {
      void player?.then((p) => {
        p?.pause();
        p?.seek(0);
      });
      return;
    }
    if (start && !reducedMotion()) start.hidden = true; // playback follows the expand: no overlay flash
    // Never autoPlay via create: a collapse before the player resolves would leave it playing hidden.
    void create(false).then((p) => {
      if (!open) return;
      const still = reducedMotion();
      if (!still) p?.play();
      root.scrollIntoView(still ? { block: 'nearest' } : { block: 'nearest', behavior: 'smooth' });
    });
  });
}
