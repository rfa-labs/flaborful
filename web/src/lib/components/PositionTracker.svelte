<script lang="ts">
  // PositionTracker is rendered INSIDE <Svelvet> so it has access to the graph context.
  // It does two things:
  //  1. Subscribes to svelvet's internal per-node `position` stores and dispatches a
  //     `position` CustomEvent whenever any node's live (dragged) position changes.
  //  2. Exposes a `fit()` method (via bind:this) that centers the graph in the viewport by
  //     reading the graph's node-bounds/dimensions stores from context and setting the
  //     transform stores — the same math svelvet's own Controls.fitView uses. We do this
  //     here (rather than via svelvet's `fitView` prop) because that prop sets `initialFit`
  //     unconditionally on first run, so when nodes mount asynchronously after the graph
  //     (our case — loaded from the API), it fires before bounds exist and never retries.
  import { getContext, onDestroy } from 'svelte';
  import { createEventDispatcher } from 'svelte';
  import { get } from 'svelte/store';

  // Replicates svelvet's calculateFitView (not in its public exports): given the graph's
  // viewport dimensions and the node bounds, returns the translation + scale that center
  // the graph in the viewport at 80% of fit. Returns nulls when bounds aren't populated yet.
  function calculateFitView(
    dimensions: { width: number; height: number },
    bounds: { top: number; left: number; right: number; bottom: number },
  ): { x: number; y: number; scale: number } | { x: null; y: null; scale: null } {
    const { width, height } = dimensions;
    const { top, left, right, bottom } = bounds;
    const boundsWidth = right - left;
    const boundsHeight = bottom - top;
    if (!boundsWidth || !boundsHeight) return { x: null, y: null, scale: null };
    const centerX = left + boundsWidth / 2;
    const centerY = top + boundsHeight / 2;
    const scale = Math.min(width / boundsWidth, height / boundsHeight) * 0.8;
    return {
      x: (width / 2 - centerX) * scale,
      y: (height / 2 - centerY) * scale,
      scale,
    };
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const graph: any = getContext('graph');
  const dispatch = createEventDispatcher<{ position: { id: string; x: number; y: number } }>();

  // Unsubscribe functions for each node's position store we're currently watching.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const unsubs: Record<string, () => void> = {};

  function trackNode(node: { id: string; position: { subscribe: (cb: (v: { x: number; y: number }) => void) => () => void } }) {
    if (unsubs[node.id]) return;
    // svelvet stores nodes internally under an `N-`-prefixed id (createNode.js), but the app
    // keys WFNode.id and livePositions by the bare id. Strip the prefix so the keys align.
    const appId = node.id.startsWith('N-') ? node.id.slice(2) : node.id;
    unsubs[node.id] = node.position.subscribe((v) => {
      dispatch('position', { id: appId, x: v.x, y: v.y });
    });
  }

  function untrackAll() {
    for (const id of Object.keys(unsubs)) {
      unsubs[id]();
      delete unsubs[id];
    }
  }

  // graph.nodes is a writable store (from createStore.js) wrapping a Map<id, node>.
  // Subscribing fires once immediately and on every add/delete (the store's `update`
  // mutates the Map in place but Svelte re-emits because it's an object reference).
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const unsubNodes = graph.nodes.subscribe((nodeMap: Map<string, any>) => {
    // Track every node currently in the graph.
    nodeMap.forEach((node) => trackNode(node));
    // Stop tracking nodes that have been removed.
    for (const id of Object.keys(unsubs)) {
      if (!nodeMap.has(id)) {
        unsubs[id]();
        delete unsubs[id];
      }
    }
  });

  // Center the graph in the viewport. Returns true if a fit was actually applied (i.e. node
  // bounds were populated), false otherwise — so callers can retry on the next tick.
  export function fit(): boolean {
    const dimensions = get(graph.dimensions) as { width: number; height: number };
    const nodeBounds = get(graph.bounds.nodeBounds) as { top: number; left: number; right: number; bottom: number };
    const { x, y, scale } = calculateFitView(dimensions, nodeBounds);
    if (x === null || y === null || scale === null) return false;
    graph.transforms.scale.set(scale);
    graph.transforms.translation.set({ x, y });
    return true;
  }

  onDestroy(() => {
    unsubNodes();
    untrackAll();
  });
</script>