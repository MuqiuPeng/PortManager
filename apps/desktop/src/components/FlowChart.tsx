import { useCallback, useEffect, useRef, useState } from "react";

import type { FlowNode } from "../types";

/** Where a node has been put, if somebody moved it. */
export type Placement = Record<string, { x: number; y: number }>;

interface Props {
  flow: FlowNode[];
  /** Positions somebody arranged. Anything absent falls back to the layout. */
  placement?: Placement;
  /** Called when a node is dropped, with its new position. */
  onMove?: (name: string, x: number, y: number) => void;
  /** Highlighted node, if one is selected. */
  selected?: string;
  onSelect?: (node: FlowNode) => void;
}

const BOX_W = 132;
const BOX_H = 34;
/** Room between stages, which is where the arrow lives. */
const GAP_X = 76;
/** Room between two things in the same stage, which need only be apart. */
const GAP_Y = 14;
const PAD = 16;

/**
 * A stack drawn as what it is: a graph.
 *
 * Stages run left to right and everything in one stage starts at once, which
 * is the fact a list of steps cannot hold — two services waiting on the same
 * database and on nothing else are not first and second, they are both.
 *
 * Across rather than down because that is the direction this kind of picture
 * is read in, and because it is the axis there is room on: a stage is at most
 * a few boxes tall, while a stack can be many stages long.
 *
 * The lines are drawn from each node's own `after`, never from which stages
 * happen to be adjacent. A service in the first stage feeding one in the third
 * gets a line that passes the second, and that crossing is the truth: it is
 * what "web waits for postgres, garage and the migration" looks like.
 *
 * Nodes can be dragged. What that moves is only where a box sits — dependencies
 * are a fact about a service, and there is one place to change them. So an
 * arrangement can end up saying nothing useful, with a node parked left of
 * something it waits for; what it cannot do is contradict the graph, because
 * the edges are still drawn between the same two boxes wherever they are put.
 *
 * There is no `viewBox` on purpose. A scaled canvas needs every pointer
 * position converted out of screen units before it means anything, and one
 * missing conversion is a node that drifts away from the cursor. At 1:1 the
 * arithmetic is subtraction.
 */
export function FlowChart({ flow, placement, onMove, selected, onSelect }: Props) {
  const surface = useRef<SVGSVGElement | null>(null);
  // Which node is being read. Kept here rather than lifted, because it changes
  // nothing outside the picture: it is the answer to "what does this one wait
  // for, and what waits for it", which is the question a graph with six edges
  // crossing each other stops being able to answer at a glance.
  const [focused, setFocused] = useState<string | null>(null);
  // Where a node is while it is being dragged, before anybody is told.
  const [dragging, setDragging] = useState<{ name: string; x: number; y: number } | null>(null);

  const laid = layout(flow);
  const where = (name: string) => {
    if (dragging?.name === name) return { x: dragging.x, y: dragging.y };
    return placement?.[name] ?? laid.get(name) ?? { x: PAD, y: PAD };
  };

  const onPointerDown = useCallback(
    (event: React.PointerEvent<SVGGElement>, name: string) => {
      if (!onMove) return;
      const box = surface.current?.getBoundingClientRect();
      if (!box) return;
      event.preventDefault();
      (event.target as Element).setPointerCapture?.(event.pointerId);
      const start = placement?.[name] ?? laid.get(name) ?? { x: PAD, y: PAD };
      // The grab point inside the box, so it does not jump to the cursor.
      const holdX = event.clientX - box.left - start.x;
      const holdY = event.clientY - box.top - start.y;
      const from = { x: event.clientX, y: event.clientY };
      // A press is not a drag until the pointer has gone somewhere. Without
      // this every click on a node wrote a position — including the one that
      // only meant to select it — so a stack picked up an arrangement nobody
      // had arranged, and the layout it should have fallen back to was gone.
      const SLOP = 4;
      let moved = false;

      const moveTo = (e: PointerEvent) => ({
        x: Math.max(0, e.clientX - box.left - holdX),
        y: Math.max(0, e.clientY - box.top - holdY),
      });

      const move = (e: PointerEvent) => {
        if (!moved) {
          if (Math.abs(e.clientX - from.x) < SLOP && Math.abs(e.clientY - from.y) < SLOP) return;
          moved = true;
          setDragging({ name, ...moveTo(e) });
          return;
        }
        setDragging({ name, ...moveTo(e) });
      };
      const up = (e: PointerEvent) => {
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", up);
        setDragging(null);
        if (moved) {
          const at = moveTo(e);
          onMove(name, at.x, at.y);
        }
      };
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", up);
    },
    [onMove, placement, laid],
  );

  // Listeners are added on the window rather than the node, because a pointer
  // moved faster than React re-renders leaves the element behind and the drag
  // would stop wherever the cursor left it.
  useEffect(
    () => () => {
      setDragging(null);
    },
    [flow],
  );

  if (flow.length === 0) return null;

  const spots = flow.map((node) => where(node.name));
  // Sized to its contents, not to whatever it is inside. Stages run across, so
  // a long stack wants width its container may not have — and a `100%` canvas
  // would shrink to fit and cut the last stage off, which is what it did.
  const width = Math.max(...spots.map((s) => s.x)) + BOX_W + PAD;
  const height = Math.max(...spots.map((s) => s.y)) + BOX_H + PAD;

  /** What the focused node touches, in either direction. */
  const touching = new Set<string>();
  if (focused) {
    touching.add(focused);
    for (const node of flow) {
      const after = node.after ?? [];
      if (node.name === focused) after.forEach((dep) => touching.add(dep));
      if (after.includes(focused)) touching.add(node.name);
    }
  }

  return (
    // Width from the contents, height from the room. Stages run across, so the
    // canvas has to be as wide as the stack is long and scroll if it is longer
    // than the band — but the height is not the picture's to decide: the space
    // under it is where a node is dragged to, and a canvas cropped to two rows
    // of boxes has nowhere to drag anything.
    //
    // Safe only because there is no `viewBox`: the viewport grows, the
    // coordinate system does not, and a pointer position still means what it
    // says without being converted out of anything.
    <svg
      ref={surface}
      className="flow"
      width={width}
      style={{ height: "100%", minHeight: height }}
    >
      <defs>
        <marker
          id="flow-arrow"
          viewBox="0 0 8 8"
          refX="7"
          refY="4"
          markerWidth="7"
          markerHeight="7"
          orient="auto-start-reverse"
        >
          <path className="flow-arrowhead" d="M 0 0 L 8 4 L 0 8 z" />
        </marker>
        <marker
          id="flow-arrow-lit"
          viewBox="0 0 8 8"
          refX="7"
          refY="4"
          markerWidth="7"
          markerHeight="7"
          orient="auto-start-reverse"
        >
          <path className="flow-arrowhead lit" d="M 0 0 L 8 4 L 0 8 z" />
        </marker>
      </defs>

      {flow.flatMap((node) =>
        (node.after ?? []).map((dep) => {
          const from = where(dep);
          const to = where(node.name);
          if (!flow.some((n) => n.name === dep)) return null;
          const x1 = from.x + BOX_W;
          const y1 = from.y + BOX_H / 2;
          // Stop short of the box so the arrowhead points at it rather than
          // sitting inside it.
          const x2 = to.x - 7;
          const y2 = to.y + BOX_H / 2;
          // Curved rather than straight so crossing lines stay tellable apart.
          const mid = (x1 + x2) / 2;
          return (
            <path
              key={`${dep}->${node.name}`}
              className={
                focused && (dep === focused || node.name === focused)
                  ? "flow-edge linked"
                  : focused
                    ? "flow-edge faded"
                    : "flow-edge"
              }
              markerEnd={
                focused && (dep === focused || node.name === focused)
                  ? "url(#flow-arrow-lit)"
                  : "url(#flow-arrow)"
              }
              d={`M ${x1} ${y1} C ${mid} ${y1}, ${mid} ${y2}, ${x2} ${y2}`}
            />
          );
        }),
      )}

      {flow.map((node) => {
        const spot = where(node.name);
        const state = node.service_id ? `flow-node status-${node.status}` : "flow-node missing";
        const held = dragging?.name === node.name;
        return (
          <g
            key={node.name}
            className={[
              state,
              selected === node.name || focused === node.name ? "selected" : "",
              held ? "held" : "",
              focused && !touching.has(node.name) ? "faded" : "",
            ]
              .filter(Boolean)
              .join(" ")}
            transform={`translate(${spot.x} ${spot.y})`}
            onPointerDown={(event) => onPointerDown(event, node.name)}
            onClick={() => {
              if (held) return;
              setFocused((was) => (was === node.name ? null : node.name));
              onSelect?.(node);
            }}
            role="button"
            style={onMove ? { cursor: held ? "grabbing" : "grab" } : undefined}
          >
            <rect width={BOX_W} height={BOX_H} rx={6} />
            <text x={10} y={BOX_H / 2 + 4}>
              {node.name.length > 14 ? `${node.name.slice(0, 13)}…` : node.name}
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

/**
 * The arrangement a stack gets before anybody rearranges it.
 *
 * One column per stage, each column centred against the tallest, which is the
 * reading order the levels already imply.
 */
function layout(flow: FlowNode[]): Map<string, { x: number; y: number }> {
  const at = new Map<string, { x: number; y: number }>();
  if (flow.length === 0) return at;

  const depth = Math.max(...flow.map((node) => node.level));
  const stages: FlowNode[][] = [];
  for (let level = 0; level <= depth; level += 1) {
    stages.push(flow.filter((node) => node.level === level));
  }
  const tallest = Math.max(...stages.map((stage) => stage.length));
  const height = tallest * BOX_H + (tallest - 1) * GAP_Y;

  stages.forEach((stage, level) => {
    const stageHeight = stage.length * BOX_H + (stage.length - 1) * GAP_Y;
    // Centred against the tallest stage, so a single box sits level with the
    // middle of the fan it came out of rather than at the top of it.
    const top = PAD + (height - stageHeight) / 2;
    stage.forEach((node, index) => {
      at.set(node.name, {
        x: PAD + level * (BOX_W + GAP_X),
        y: top + index * (BOX_H + GAP_Y),
      });
    });
  });
  return at;
}
