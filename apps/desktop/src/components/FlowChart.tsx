import type { FlowNode } from "../types";

interface Props {
  flow: FlowNode[];
  /** Highlighted node, if one is selected. */
  selected?: string;
  onSelect?: (node: FlowNode) => void;
}

const BOX_W = 116;
const BOX_H = 30;
const GAP_X = 18;
const GAP_Y = 34;
const PAD = 8;

/**
 * A group drawn as what it is: a graph.
 *
 * Levels run down the page and everything on one level starts at once, which
 * is the fact a list of steps cannot hold — two services waiting on the same
 * database and on nothing else are not first and second, they are both.
 *
 * Laid out here rather than by a graph library: the shapes are small, the
 * levels come from the daemon already assigned, and the whole layout is
 * "centre each row, draw a line to each thing you wait for".
 */
export function FlowChart({ flow, selected, onSelect }: Props) {
  if (flow.length === 0) return null;

  const depth = Math.max(...flow.map((node) => node.level));
  const rows: FlowNode[][] = [];
  for (let level = 0; level <= depth; level += 1) {
    rows.push(flow.filter((node) => node.level === level));
  }
  const widest = Math.max(...rows.map((row) => row.length));
  const width = widest * BOX_W + (widest - 1) * GAP_X + PAD * 2;
  const height = rows.length * BOX_H + (rows.length - 1) * GAP_Y + PAD * 2;

  const at = new Map<string, { x: number; y: number }>();
  rows.forEach((row, level) => {
    const rowWidth = row.length * BOX_W + (row.length - 1) * GAP_X;
    const left = (width - rowWidth) / 2;
    row.forEach((node, index) => {
      at.set(node.name, {
        x: left + index * (BOX_W + GAP_X),
        y: PAD + level * (BOX_H + GAP_Y),
      });
    });
  });

  return (
    <svg className="flow" viewBox={`0 0 ${width} ${height}`} width={width} height={height}>
      {flow.flatMap((node) =>
        (node.after ?? []).map((dep) => {
          const from = at.get(dep);
          const to = at.get(node.name);
          if (!from || !to) return null;
          const x1 = from.x + BOX_W / 2;
          const y1 = from.y + BOX_H;
          const x2 = to.x + BOX_W / 2;
          const y2 = to.y;
          // Curved rather than straight so crossing lines stay tellable apart.
          const mid = (y1 + y2) / 2;
          return (
            <path
              key={`${dep}->${node.name}`}
              className="flow-edge"
              d={`M ${x1} ${y1} C ${x1} ${mid}, ${x2} ${mid}, ${x2} ${y2}`}
            />
          );
        }),
      )}

      {flow.map((node) => {
        const spot = at.get(node.name)!;
        const state = node.service_id
          ? `flow-node status-${node.status}`
          : "flow-node missing";
        return (
          <g
            key={node.name}
            className={selected === node.name ? `${state} selected` : state}
            transform={`translate(${spot.x} ${spot.y})`}
            onClick={() => onSelect?.(node)}
            role={onSelect ? "button" : undefined}
          >
            <rect width={BOX_W} height={BOX_H} rx={6} />
            <text x={10} y={BOX_H / 2 + 4}>
              {node.name.length > 13 ? `${node.name.slice(0, 12)}…` : node.name}
            </text>
            {node.one_shot && (
              <text className="flow-tag" x={BOX_W - 8} y={BOX_H / 2 + 4} textAnchor="end">
                once
              </text>
            )}
          </g>
        );
      })}
    </svg>
  );
}
