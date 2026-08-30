import styles from "./Switch.module.scss";

export function Switch({ checked, disabled, label, onChange }: { checked: boolean; disabled?: boolean; label: string; onChange: (checked: boolean) => void }) {
  return <button
    type="button"
    role="switch"
    aria-checked={checked}
    aria-label={label}
    disabled={disabled}
    className={styles.root}
    data-checked={checked || undefined}
    onClick={() => onChange(!checked)}
  ><span /></button>;
}
