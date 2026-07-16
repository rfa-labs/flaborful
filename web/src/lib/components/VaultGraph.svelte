<script lang="ts">
  // Obsidian-style force-directed knowledge-vault graph. Uses d3-force for the physics (nodes
  // repel, edges are springs) rendered to a canvas, with drag (the whole web reflows live),
  // zoom/pan, and neighbor-highlighting on hover/select. Same props/shape as before, so both the
  // company application-review view (locked/anonymized nodes) and the professional's own studio
  // view (full notes) reuse it unchanged.
  import { onMount, onDestroy } from 'svelte';
  import {
    forceSimulation, forceManyBody, forceLink, forceCenter, forceCollide, forceX, forceY,
    type Simulation,
  } from 'd3-force';
  import { select } from 'd3-selection';
  import { drag as d3drag } from 'd3-drag';
  import { zoom as d3zoom, zoomIdentity, type ZoomTransform } from 'd3-zoom';
  import type { VaultGraph, VaultGraphNode } from '$lib/api/types';

  export let graph: VaultGraph;
  export let height = 460;

  // Functional category colors — they distinguish note layers at a glance, so most are left as
  // meaningful hues. Only the brand-default (style_rule) and the neutral note/locked greys are
  // aligned to the new warm palette.
  const layerColor: Record<string, string> = {
    style_rule: '#7c4dff',
    golden_snippet: '#f59e0b',
    architecture: '#2dd4bf',
    feature: '#38bdf8',
    pattern: '#60a5fa',
    tech: '#34d399',
    note: '#b4a396',
  };
  const LOCKED = '#b4a396';

  // Simulation node/link shapes (d3 mutates x/y/vx/vy onto these).
  interface SimNode extends VaultGraphNode { x: number; y: number; vx?: number; vy?: number; fx?: number | null; fy?: number | null; deg: number }
  interface SimLink { source: SimNode | string; target: SimNode | string }

  let canvas: HTMLCanvasElement;
  let wrap: HTMLDivElement;
  let width = 800;
  let sim: Simulation<SimNode, SimLink> | null = null;
  let transform: ZoomTransform = zoomIdentity;
  let zoomBehavior: ReturnType<typeof d3zoom<HTMLCanvasElement, unknown>> | null = null;
  let nodes: SimNode[] = [];
  let links: SimLink[] = [];
  let neighbors = new Map<string, Set<string>>();
  let hovered: SimNode | null = null;
  let selected: VaultGraphNode | null = null;
  let dpr = 1;

  const sharedCount = () => graph.nodes.filter((n) => !n.locked).length;

  function buildModel() {
    // Degree (connection count) → node size, like Obsidian.
    const deg = new Map<string, number>();
    neighbors = new Map();
    for (const n of graph.nodes) { deg.set(n.slug, 0); neighbors.set(n.slug, new Set()); }
    for (const e of graph.edges) {
      deg.set(e.source, (deg.get(e.source) ?? 0) + 1);
      deg.set(e.target, (deg.get(e.target) ?? 0) + 1);
      neighbors.get(e.source)?.add(e.target);
      neighbors.get(e.target)?.add(e.source);
    }
    nodes = graph.nodes.map((n) => ({ ...n, x: width / 2 + (Math.random() - 0.5) * 200, y: height / 2 + (Math.random() - 0.5) * 200, deg: deg.get(n.slug) ?? 0 }));
    links = graph.edges.map((e) => ({ source: e.source, target: e.target }));
  }

  // Node size scales with connection count (degree), like Obsidian. Square-root scaling keeps
  // hubs visibly larger without one mega-node dwarfing everything (sqrt = area-proportional).
  const radius = (n: SimNode) => 4 + Math.sqrt(n.deg) * 2.4;

  let didFit = false;
  function startSim() {
    sim?.stop();
    didFit = false;
    sim = forceSimulation<SimNode, SimLink>(nodes)
      .force('charge', forceManyBody().strength(-280))
      .force('link', forceLink<SimNode, SimLink>(links).id((d) => d.slug).distance(80).strength(0.5))
      .force('center', forceCenter(width / 2, height / 2))
      .force('collide', forceCollide<SimNode>().radius((d) => radius(d) + 10))
      // Gentle pull toward center keeps disconnected nodes from drifting off to infinity (they
      // have no links to anchor them), so the graph stays compact and framable.
      .force('x', forceX(width / 2).strength(0.04))
      .force('y', forceY(height / 2).strength(0.04))
      .on('tick', () => {
        // Once the layout has cooled, frame the WHOLE graph so no cluster (e.g. UMS) is left
        // off-screen. Do it once, when alpha drops below a settled threshold.
        if (!didFit && sim && sim.alpha() < 0.08) { didFit = true; fitView(); }
        draw();
      });
  }

  // Set the zoom/pan transform so every node fits inside the canvas with a margin.
  function fitView() {
    if (nodes.length === 0) return;
    // Fit to where the BULK of the graph is, not the absolute extremes — a few disconnected nodes
    // can drift far out and, if included, shrink the real graph to a dot. Clip to the 2nd–98th
    // percentile of x/y so outliers don't dominate the frame.
    const xs = nodes.map((n) => n.x).sort((a, b) => a - b);
    const ys = nodes.map((n) => n.y).sort((a, b) => a - b);
    const q = (arr: number[], p: number) => arr[Math.min(arr.length - 1, Math.max(0, Math.floor(p * (arr.length - 1))))];
    const minX = q(xs, 0.02), maxX = q(xs, 0.98), minY = q(ys, 0.02), maxY = q(ys, 0.98);
    const gw = maxX - minX || 1, gh = maxY - minY || 1;
    const pad = 50;
    const k = Math.max(0.05, Math.min((width - pad * 2) / gw, (height - pad * 2) / gh, 3));
    const tx = (width - k * (minX + maxX)) / 2;
    const ty = (height - k * (minY + maxY)) / 2;
    transform = zoomIdentity.translate(tx, ty).scale(k);
    // Keep the d3-zoom behavior's internal transform in sync so user zoom/pan continues smoothly.
    if (zoomBehavior) select(canvas).call(zoomBehavior.transform, transform);
    draw();
  }

  function isHighlighted(slug: string): boolean {
    const anchor = hovered?.slug ?? selected?.slug;
    if (!anchor) return true; // nothing focused → everything full opacity
    return slug === anchor || (neighbors.get(anchor)?.has(slug) ?? false);
  }

  function draw() {
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    ctx.save();
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.scale(dpr, dpr);
    ctx.translate(transform.x, transform.y);
    ctx.scale(transform.k, transform.k);

    const anchor = hovered?.slug ?? selected?.slug;

    // Edges
    for (const l of links) {
      const s = l.source as SimNode, t = l.target as SimNode;
      if (!s.x || !t.x) continue;
      const lit = !anchor || (isHighlighted(s.slug) && isHighlighted(t.slug));
      ctx.beginPath();
      ctx.moveTo(s.x, s.y);
      ctx.lineTo(t.x, t.y);
      ctx.strokeStyle = lit ? 'rgba(148,163,184,0.55)' : 'rgba(148,163,184,0.08)';
      ctx.lineWidth = 1 / transform.k;
      ctx.stroke();
    }

    // Nodes (circles)
    for (const n of nodes) {
      const lit = isHighlighted(n.slug);
      const r = radius(n);
      ctx.globalAlpha = lit ? 1 : 0.15;
      ctx.beginPath();
      ctx.arc(n.x, n.y, r, 0, Math.PI * 2);
      ctx.fillStyle = n.locked ? LOCKED : (layerColor[n.layer] ?? layerColor.note);
      ctx.fill();
      if (anchor === n.slug) { ctx.lineWidth = 2 / transform.k; ctx.strokeStyle = '#fff'; ctx.stroke(); }
    }

    // Labels — decluttered. Rather than spacing nodes so far apart that labels never touch (which
    // would shrink the whole graph), we draw labels greedily by importance (most-connected first)
    // and SKIP any whose box would overlap one already placed. So you always see the labels that
    // fit, no two ever collide, and more appear as you zoom in.
    const fontPx = 11 / transform.k;
    ctx.font = `${fontPx}px ui-sans-serif, system-ui, sans-serif`;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    const placed: { x: number; y: number; w: number; h: number }[] = [];
    const pad = 2 / transform.k;
    // Labels stay hidden until zoomed in past this threshold — keeps the overview clean. The only
    // exception is the focused node + its neighbors (hover/select), which are always labeled so
    // the highlight interaction is readable at any zoom.
    const LABEL_ZOOM = 2.4;
    const candidates = [...nodes]
      .filter((n) => isHighlighted(n.slug))
      .sort((a, b) => (anchor === b.slug ? 1 : 0) - (anchor === a.slug ? 1 : 0) || b.deg - a.deg);

    for (const n of candidates) {
      // Below the zoom threshold, only label focus/neighbors; above it, every node competes for
      // label space via collision detection.
      if (!anchor && transform.k < LABEL_ZOOM) continue;
      const r = radius(n);
      const raw = n.locked ? '🔒' : n.title;
      const label = raw.length > 26 ? raw.slice(0, 25) + '…' : raw;
      const w = ctx.measureText(label).width;
      const h = fontPx;
      const lx = n.x;
      const ly = n.y + r + fontPx * 0.9;
      const box = { x: lx - w / 2 - pad, y: ly - h / 2 - pad, w: w + pad * 2, h: h + pad * 2 };
      const overlaps = placed.some(
        (p) => box.x < p.x + p.w && box.x + box.w > p.x && box.y < p.y + p.h && box.y + box.h > p.y,
      );
      if (overlaps) continue;
      placed.push(box);
      ctx.globalAlpha = isHighlighted(n.slug) ? 0.92 : 0.25;
      ctx.fillStyle = '#ece0d6';
      ctx.fillText(label, lx, ly);
    }
    ctx.restore();
  }

  // Map screen coords → graph coords and find the node under the pointer.
  function nodeAt(px: number, py: number): SimNode | null {
    const gx = (px - transform.x) / transform.k;
    const gy = (py - transform.y) / transform.k;
    for (let i = nodes.length - 1; i >= 0; i--) {
      const n = nodes[i];
      const r = radius(n) + 3;
      if ((n.x - gx) ** 2 + (n.y - gy) ** 2 <= r * r) return n;
    }
    return null;
  }

  function resize() {
    if (!wrap || !canvas) return;
    width = wrap.clientWidth;
    dpr = window.devicePixelRatio || 1;
    canvas.width = width * dpr;
    canvas.height = height * dpr;
    canvas.style.width = `${width}px`;
    canvas.style.height = `${height}px`;
    sim?.force('center', forceCenter(width / 2, height / 2));
    draw();
  }

  let mounted = false;

  function rebuild() {
    if (!mounted) return;
    buildModel();
    resize();
    startSim();
    draw();
  }

  onMount(() => {
    mounted = true;
    buildModel();
    resize();
    startSim();

    const sel = select(canvas);

    // Drag a node — fix it under the pointer; the rest of the web reflows live.
    sel.call(
      d3drag<HTMLCanvasElement, unknown>()
        .container(canvas)
        .subject((event) => {
          const [px, py] = [event.x, event.y];
          const n = nodeAt(px, py);
          return n ?? undefined;
        })
        .on('start', (event) => {
          if (!event.active) sim?.alphaTarget(0.3).restart();
          const n = event.subject as SimNode;
          n.fx = (event.x - transform.x) / transform.k;
          n.fy = (event.y - transform.y) / transform.k;
        })
        .on('drag', (event) => {
          const n = event.subject as SimNode;
          n.fx = (event.x - transform.x) / transform.k;
          n.fy = (event.y - transform.y) / transform.k;
        })
        .on('end', (event) => {
          if (!event.active) sim?.alphaTarget(0);
          const n = event.subject as SimNode;
          n.fx = null; n.fy = null;
        }) as any
    );

    // Zoom / pan. Hoisted so fitView() can drive the same behavior's transform.
    zoomBehavior = d3zoom<HTMLCanvasElement, unknown>()
      .scaleExtent([0.05, 6])
      .filter((event) => {
        // Let drag handle left-button-on-node; zoom handles wheel + background drag.
        if (event.type === 'wheel') return true;
        return !nodeAt(event.offsetX ?? event.layerX, event.offsetY ?? event.layerY);
      })
      .on('zoom', (event) => { transform = event.transform; draw(); });
    sel.call(zoomBehavior as any);

    window.addEventListener('resize', resize);
  });

  function onMove(e: MouseEvent) {
    const rect = canvas.getBoundingClientRect();
    const n = nodeAt(e.clientX - rect.left, e.clientY - rect.top);
    if (n !== hovered) { hovered = n; canvas.style.cursor = n ? 'pointer' : 'grab'; draw(); }
  }
  function onClick(e: MouseEvent) {
    const rect = canvas.getBoundingClientRect();
    const n = nodeAt(e.clientX - rect.left, e.clientY - rect.top);
    if (n) selected = n;
  }
  function onLeave() { if (hovered) { hovered = null; draw(); } }

  // Rebuild the simulation when the graph prop changes (e.g. switching applicants).
  $: if (graph) rebuild();

  onDestroy(() => { sim?.stop(); window.removeEventListener('resize', resize); });
</script>

<div class="rounded-lg border border-sand-800 overflow-hidden bg-[#0f1117]">
  <div class="flex items-center justify-between px-3 py-2 border-b border-sand-800 bg-[#15171e]">
    <span class="text-xs font-semibold text-sand-300 uppercase tracking-wide">Knowledge vault graph</span>
    <div class="flex items-center gap-3">
      <span class="text-[11px] text-sand-500">{graph.nodes.length} notes · {sharedCount()} shared · {graph.edges.length} links</span>
      <button class="text-[11px] text-sand-400 hover:text-sand-200" on:click={fitView}>Fit ⤢</button>
    </div>
  </div>

  {#if graph.nodes.length === 0}
    <div class="p-8 text-center text-sm text-sand-500">No vault notes yet.</div>
  {:else}
    <div bind:this={wrap} class="relative" style="height: {height}px">
      <canvas
        bind:this={canvas}
        on:mousemove={onMove}
        on:click={onClick}
        on:mouseleave={onLeave}
        class="block w-full cursor-grab"
      ></canvas>
      <div class="absolute bottom-2 left-3 text-[10px] text-sand-600 pointer-events-none">
        drag nodes · scroll to zoom · hover to highlight neighbors
      </div>
    </div>

    {#if selected}
      <div class="border-t border-sand-800 p-3 bg-[#15171e]">
        {#if selected.locked}
          <div class="text-sm text-sand-400">
            🔒 <span class="font-medium">{selected.title}</span> — this note wasn't shared with you.
            <span class="text-sand-600">({selected.layer.replace(/_/g, ' ')})</span>
          </div>
        {:else}
          <div class="flex items-center gap-2 mb-1">
            <span class="text-sm font-semibold text-sand-100">{selected.title}</span>
            <span class="text-[10px] uppercase tracking-wide text-sand-900 px-1.5 py-0.5 rounded"
              style="background: {layerColor[selected.layer] ?? layerColor.note}">{selected.layer.replace(/_/g, ' ')}</span>
            {#if selected.domain}<span class="text-[10px] text-sand-500">{selected.domain}</span>{/if}
            <button class="ml-auto text-xs text-sand-500 hover:text-sand-300" on:click={() => (selected = null)}>✕</button>
          </div>
          {#if selected.body}
            <pre class="whitespace-pre-wrap break-words text-sm text-sand-300 max-h-72 overflow-y-auto">{selected.body}</pre>
          {/if}
        {/if}
      </div>
    {/if}
  {/if}
</div>
