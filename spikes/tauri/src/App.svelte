<script>
  import { onMount, tick } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';

  const ROW = 18;
  // WebKit caps layout heights near 2^25 px, so the spacer is capped and the
  // scroll position maps proportionally onto the line range.
  const MAX_SPACER = 8_000_000;

  let method = $state('GET');
  let url = $state('');
  let body = $state('');
  let status = $state('');
  let lineCount = $state(0);
  let top = $state(0);
  let scrollY = $state(0);
  let rows = $state([]);
  let viewport;
  let viewportHeight = $state(400);
  let exitOn = null;
  let seq = 0;
  let cfg_debug = false;
  let scrollTo = null;
  let cfg_noevent = false;
  const t_boot = performance.now();
  const vis = [];
  document.addEventListener('visibilitychange', () => vis.push(`${document.visibilityState}@${(performance.now() - t_boot).toFixed(0)}`));
  const visLine = () => `visibility-changes ${vis.length ? vis.join(' ') : 'none'}`;

  let spacer = $derived(Math.min(lineCount * ROW, MAX_SPACER));
  let visible = $derived(Math.ceil(viewportHeight / ROW) + 1);

  function frame() {
    return new Promise((r) => requestAnimationFrame(r));
  }

  async function load() {
    const s = ++seq;
    if (!viewport) return;
    const maxScroll = Math.max(1, spacer - viewportHeight);
    const maxTop = Math.max(0, lineCount - Math.floor(viewportHeight / ROW));
    const t = spacer < lineCount * ROW
      ? Math.round((viewport.scrollTop / maxScroll) * maxTop)
      : Math.floor(viewport.scrollTop / ROW);
    const got = await invoke('lines', { start: t, count: visible });
    if (s !== seq) return;
    top = t;
    scrollY = viewport.scrollTop;
    rows = got;
  }

  async function send() {
    status = 'sending...';
    let ticking = true;
    let last = performance.now();
    let maxGap = 0;
    const t0 = last;
    const gaps = [];
    const loop = (now) => {
      if (now - last > 50) gaps.push(`${(last - t0).toFixed(0)}+${(now - last).toFixed(0)}`);
      maxGap = Math.max(maxGap, now - last);
      last = now;
      if (ticking) requestAnimationFrame(loop);
    };
    requestAnimationFrame(loop);
    try {
      const r = await invoke('send', { method, url, body });
      const tS = performance.now() - t0;
      status = `${r.status} · ${r.elapsed_ms} ms · ${r.bytes} bytes`;
      lineCount = r.line_count;
      await tick();
      viewport.scrollTop = 0;
      await load();
      const tL = performance.now() - t0;
      await tick();
      await frame();
      const tF1 = performance.now() - t0;
      await frame();
      const tF2 = performance.now() - t0;
      ticking = false;
      if (cfg_debug) gaps.push(`| send ${tS.toFixed(0)} lines ${tL.toFixed(0)} f1 ${tF1.toFixed(0)} f2 ${tF2.toFixed(0)}`);
      if (scrollTo != null) {
        viewport.scrollTop = scrollTo * (viewport.scrollHeight - viewport.clientHeight);
        await load();
      }
      if (exitOn === 'scroll') {
        const times = [];
        for (let i = 0; i <= 40; i++) {
          const ts = performance.now();
          viewport.scrollTop = ((i * 7919) % 41) / 40 * (viewport.scrollHeight - viewport.clientHeight);
          await load();
          await tick();
          await frame();
          times.push(performance.now() - ts);
        }
        times.sort((a, b) => a - b);
        invoke('report', { lines: [`scroll-jump-to-frame median ${times[20].toFixed(1)} ms, max ${times[40].toFixed(1)} ms over 41 jumps`, `last-top ${top} rows ${rows.length} of ${lineCount}`] });
      }
      if (exitOn === 'viewer-ready' && rows.length > 0) {
        invoke('report', { lines: ['viewer-ready {ms}', `max-frame-gap ${maxGap.toFixed(1)}`, visLine(), ...(cfg_debug ? [`gaps-over-50ms ${gaps.join(' ')}`] : [])] });
      }
    } catch (e) {
      ticking = false;
      status = String(e);
      if (exitOn === 'viewer-ready') invoke('report', { lines: [`error ${e}`] });
    }
  }

  async function editorBench(bytes) {
    const line = '  "key": "value value value value value value value value",\n';
    body = line.repeat(Math.ceil(bytes / line.length)).slice(0, bytes);
    await tick();
    await frame();
    await frame();
    const ta = document.querySelector('textarea');
    ta.focus();
    const times = [];
    dbg('editor-filled');
    for (let i = 0; i < 60; i++) {
      dbg(`key ${i}`);
      const pos = Math.floor(bytes / 2) + i;
      const t0 = performance.now();
      ta.setRangeText('a', pos, pos, 'end');
      if (!cfg_noevent) ta.dispatchEvent(new Event('input', { bubbles: true }));
      await frame();
      await frame();
      times.push(performance.now() - t0);
    }
    times.sort((a, b) => a - b);
    invoke('report', {
      lines: [
        `editor-bytes ${body.length}`,
        `keystroke-to-second-frame median ${times[30].toFixed(1)} ms, max ${times[59].toFixed(1)} ms`,
        visLine(),
      ],
    });
  }

  const dbg = (msg) => { if (cfg_debug) invoke('log', { msg }); };

  onMount(async () => {
    const ro = new ResizeObserver(() => { viewportHeight = viewport.clientHeight; load(); });
    ro.observe(viewport);
    const cfg = await invoke('config');
    if (cfg.debug) invoke('log', { msg: 'config-received' });
    exitOn = cfg.exit_on;
    cfg_debug = cfg.debug;
    scrollTo = cfg.scroll_to;
    cfg_noevent = cfg.editor_no_event;
    requestAnimationFrame(() => requestAnimationFrame(() => {
      if (exitOn === 'first-frame') invoke('report', { lines: ['first-frame {ms}', visLine()] });
    }));
    if (cfg.url) {
      url = cfg.url;
      if (cfg.send_after_paint) { await frame(); await frame(); }
      send();
    }
    if (exitOn === 'ticks') {
      const t0 = performance.now(); let last = t0; const gaps = [];
      const loop = (now) => {
        if (now - last > 50) gaps.push(`${(last - t0).toFixed(0)}+${(now - last).toFixed(0)}`);
        last = now;
        if (now - t0 < 6000) requestAnimationFrame(loop);
        else invoke('report', { lines: [`mount {ms}`, `ticks-gaps ${gaps.join(' ')}`, visLine()] });
      };
      requestAnimationFrame(loop);
    }
    if (exitOn === 'editor') editorBench(cfg.editor_bytes ?? 1 << 20);
  });
</script>

<main>
  <div class="top">
    <input class="method" bind:value={method} />
    <input class="url" bind:value={url} placeholder="http://127.0.0.1:8703/array.json" />
    <button onclick={send}>Send</button>
  </div>
  <textarea bind:value={body} placeholder="request body" spellcheck="false"></textarea>
  <div class="status">{status}</div>
  <div class="viewer" bind:this={viewport} onscroll={load}>
    <div style="height:{spacer}px"></div>
    {#each rows as text, i (i)}
      <div class="row" style="top:{scrollY + i * ROW}px">{text}</div>
    {/each}
  </div>
  <div class="foot">lines {top + 1}-{top + rows.length} of {lineCount}</div>
</main>

<style>
  :global(html, body) { margin: 0; height: 100%; font-family: -apple-system, sans-serif; font-size: 13px; }
  :global(#app) { height: 100%; }
  main { display: flex; flex-direction: column; height: 100%; padding: 8px; box-sizing: border-box; gap: 6px; }
  .top { display: flex; gap: 6px; }
  .method { width: 70px; }
  .url { flex: 1; }
  textarea { height: 120px; font-family: Menlo, monospace; font-size: 12px; resize: vertical; }
  .status { font-family: Menlo, monospace; }
  .viewer { flex: 1; overflow-y: auto; position: relative; border: 1px solid #ccc; font-family: Menlo, monospace; font-size: 12px; }
  .row { position: absolute; left: 4px; right: 0; height: 18px; line-height: 18px; white-space: pre; }
  .foot { color: #666; font-size: 11px; }
</style>
