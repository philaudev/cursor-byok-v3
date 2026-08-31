import { autoUpdate, computePosition, flip, offset, shift, type VirtualElement } from "@floating-ui/dom";
import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import styles from "./Tooltip.module.scss";

export type TooltipAnchor = VirtualElement;

export function Tooltip({ id, anchor, children }: { id?: string; anchor: TooltipAnchor | null; children: ReactNode }) {
  const tooltipRef = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);

  useLayoutEffect(() => {
    const tooltip = tooltipRef.current;
    if (!anchor || !tooltip) {
      setPosition(null);
      return;
    }

    const updatePosition = () => {
      void computePosition(anchor, tooltip, {
        placement: "top",
        middleware: [offset(8), flip({ padding: 12 }), shift({ padding: 12 })],
      }).then(({ x, y }) => setPosition({ left: x, top: y }));
    };

    return autoUpdate(anchor, tooltip, updatePosition);
  }, [anchor]);

  if (!anchor) return null;

  // Tooltip overlays always live under body, outside the trigger's layout and stacking context.
  return createPortal(
    <div
      ref={tooltipRef}
      id={id}
      className={styles.root}
      role="tooltip"
      style={{ left: position?.left ?? 0, top: position?.top ?? 0, visibility: position ? "visible" : "hidden" }}
    >
      {children}
    </div>,
    document.body,
  );
}
