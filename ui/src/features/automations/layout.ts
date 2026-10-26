import dagre from "@dagrejs/dagre";
import type { WfDef } from "./types";

export const NODE_W = 220;
export const NODE_H = 64;

/** Positions for every node, left to right. Used for "Tidy" and for a workflow that has no positions. */
export function tidyPositions(def: WfDef): Record<string, { x: number; y: number }> {
  const g = new dagre.graphlib.Graph();
  g.setGraph({ rankdir: "LR", nodesep: 36, ranksep: 90, marginx: 20, marginy: 20 });
  g.setDefaultEdgeLabel(() => ({}));
  for (const n of def.nodes) g.setNode(n.id, { width: NODE_W, height: NODE_H + portExtra(def, n.id) });
  for (const e of def.edges) g.setEdge(e.from, e.to);
  dagre.layout(g);
  const out: Record<string, { x: number; y: number }> = {};
  for (const n of def.nodes) {
    const p = g.node(n.id);
    if (p) out[n.id] = { x: Math.round(p.x - NODE_W / 2), y: Math.round(p.y - p.height / 2) };
  }
  return out;
}

/** A node with many ports is taller. */
function portExtra(def: WfDef, id: string): number {
  const n = def.nodes.find((x) => x.id === id);
  if (n?.type === "flow.switch") {
    const cases = Array.isArray(n.params?.cases) ? (n.params.cases as unknown[]).length : 2;
    return Math.max(0, cases - 1) * 14;
  }
  return 0;
}

export function withTidyPositions(def: WfDef): WfDef {
  const pos = tidyPositions(def);
  return { ...def, nodes: def.nodes.map((n) => ({ ...n, position: pos[n.id] ?? n.position ?? { x: 0, y: 0 } })) };
}

/** True if some node has no position (so the editor must lay the graph out once). */
export function needsLayout(def: WfDef): boolean {
  return def.nodes.some((n) => !n.position);
}
