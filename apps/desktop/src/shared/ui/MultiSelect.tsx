import { autoUpdate, computePosition, flip, offset, shift, size } from "@floating-ui/dom";
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Icon, type IconProps } from "./Icon";
import { checkIcon, chevronDownIcon, windowCloseIcon } from "./icons";
import { VirtualList } from "../virtual/VirtualList";
import styles from "./MultiSelect.module.scss";

export type MultiSelectOption = {
  value: string;
  label: string;
  icon?: IconProps["icon"];
};

export function MultiSelect({ label, value, options, onChange }: {
  label: string;
  value: string[];
  options: MultiSelectOption[];
  onChange: (value: string[]) => void;
}) {
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const firstOption = useRef<HTMLButtonElement>(null);
  const menuId = useId();
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState({ left: 0, top: 0, width: 220, maxHeight: 280 });

  useLayoutEffect(() => {
    if (!open || !root.current || !menu.current) return;
    return autoUpdate(root.current, menu.current, () => void computePosition(root.current!, menu.current!, {
      placement: "bottom-start",
      middleware: [offset(5), flip({ padding: 10 }), shift({ padding: 10 }), size({
        padding: 10,
        apply: ({ rects, availableHeight }) => setPosition((current) => ({
          ...current,
          width: rects.reference.width,
          maxHeight: Math.max(120, availableHeight),
        })),
      })],
    }).then(({ x, y }) => setPosition((current) => ({ ...current, left: x, top: y }))));
  }, [open]);

  useEffect(() => {
    if (!open) return;
    firstOption.current?.focus();
    const closeOutside = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!root.current?.contains(target) && !menu.current?.contains(target)) setOpen(false);
    };
    document.addEventListener("pointerdown", closeOutside);
    return () => document.removeEventListener("pointerdown", closeOutside);
  }, [open]);

  const selected = new Set(value);
  const summary = value.length === 0 ? t("全部") : t("已选 {count} 项", { count: value.length });
  const toggle = (option: string) => onChange(selected.has(option)
    ? value.filter((item) => item !== option)
    : [...value, option]);

  const close = () => { setOpen(false); trigger.current?.focus(); };
  const openMenu = () => {
    const width = root.current?.getBoundingClientRect().width;
    if (width) setPosition((current) => ({ ...current, width }));
    setOpen(true);
  };

  return <>
    <div ref={root} className={styles.target} data-open={open || undefined}>
      <button
        ref={trigger}
        type="button"
        className={styles.trigger}
        aria-label={t("筛选项：{label}，{summary}", { label, summary })}
        aria-haspopup="listbox"
        aria-controls={open ? menuId : undefined}
        aria-expanded={open}
        onClick={() => { if (open) close(); else openMenu(); }}
        onKeyDown={(event) => { if (event.key === "ArrowDown") { event.preventDefault(); openMenu(); } }}
      >
        <span>{label}</span><span className={styles.summary} aria-hidden="true">{summary}</span><Icon icon={chevronDownIcon} size="1.1em" />
      </button>
      {value.length > 0 && <button type="button" className={styles.clear} aria-label={t("清除{label}筛选", { label })} onClick={() => onChange([])}>
        <Icon icon={windowCloseIcon} size="1.1em" />
      </button>}
    </div>
    {open && createPortal(<div
      id={menuId}
      ref={menu}
      className={styles.menu}
      aria-label={label}
      style={position}
      onPointerDown={(event) => event.stopPropagation()}
      onKeyDown={(event) => { if (event.key === "Escape") { event.preventDefault(); close(); } }}
    >
      <div
        className={styles.listFrame}
        role="listbox"
        aria-label={label}
        aria-multiselectable="true"
        style={{ height: Math.min(options.length * 32, Math.max(32, position.maxHeight - 45)) }}
      ><VirtualList
          items={options}
          itemKey="value"
          className={styles.list}
          style={{ height: "100%" }}
          estimatedItemHeight={32}
          overscan={4}
          empty={<div className={styles.empty}>{t("暂无选项")}</div>}
        >{(option, index) => <button
          ref={index === 0 ? firstOption : undefined}
          type="button"
          role="option"
          aria-label={option.label}
          aria-selected={selected.has(option.value)}
          onClick={() => toggle(option.value)}
        >
          <span className={styles.option}>{option.icon && <Icon icon={option.icon} />}<span>{option.label}</span></span>
          <span className={styles.check} aria-hidden="true">{selected.has(option.value) && <Icon icon={checkIcon} size="1.1em" />}</span>
          </button>}</VirtualList></div>
      {options.length > 0 && <div className={styles.footer}>
        <button type="button" onClick={() => onChange(options.map((option) => option.value))}>{t("全选")}</button>
        <button type="button" onClick={() => onChange([])}>{t("全不选")}</button>
      </div>}
    </div>, document.body)}
  </>;
}
