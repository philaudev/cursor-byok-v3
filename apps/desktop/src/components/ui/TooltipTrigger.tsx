import { cloneElement, useEffect, useId, useState, type FocusEventHandler, type PointerEventHandler, type ReactElement } from "react";
import { Tooltip, type TooltipAnchor } from "./Tooltip";

function anchorFor(element: HTMLElement): TooltipAnchor {
  return { contextElement: element, getBoundingClientRect: () => element.getBoundingClientRect() };
}

type TriggerProps = {
  "aria-describedby"?: string;
  onPointerMove?: PointerEventHandler<HTMLElement>;
  onPointerLeave?: PointerEventHandler<HTMLElement>;
  onPointerDown?: PointerEventHandler<HTMLElement>;
  onFocus?: FocusEventHandler<HTMLElement>;
  onBlur?: FocusEventHandler<HTMLElement>;
};

export function TooltipTrigger({ label, children }: { label: string; children: ReactElement<TriggerProps> }) {
  const [anchor, setAnchor] = useState<TooltipAnchor | null>(null);
  const tooltipId = useId();
  useEffect(() => {
    if (!anchor) return;
    const close = (event: KeyboardEvent) => { if (event.key === "Escape") setAnchor(null); };
    document.addEventListener("keydown", close);
    return () => document.removeEventListener("keydown", close);
  }, [anchor]);
  const trigger = cloneElement(children, {
    "aria-describedby": [children.props["aria-describedby"], anchor ? tooltipId : null].filter(Boolean).join(" ") || undefined,
    onPointerMove: (event) => {
      children.props.onPointerMove?.(event);
      if (event.pointerType !== "touch" && event.buttons === 0) {
        const element = event.currentTarget;
        setAnchor((current) => current ?? anchorFor(element));
      }
    },
    onPointerLeave: (event) => {
      children.props.onPointerLeave?.(event);
      setAnchor(null);
    },
    onPointerDown: (event) => {
      children.props.onPointerDown?.(event);
      setAnchor(null);
    },
    onFocus: (event) => {
      children.props.onFocus?.(event);
      setAnchor(event.currentTarget.matches(":focus-visible") ? anchorFor(event.currentTarget) : null);
    },
    onBlur: (event) => {
      children.props.onBlur?.(event);
      setAnchor(null);
    },
  });

  return <>
    {trigger}
    <Tooltip id={tooltipId} anchor={anchor}>{label}</Tooltip>
  </>;
}
