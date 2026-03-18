interface ToggleProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  "aria-label"?: string;
}

export function Toggle({
  checked,
  onChange,
  disabled,
  "aria-label": ariaLabel,
}: ToggleProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={ariaLabel}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`relative h-5 w-9 flex-shrink-0 rounded-full transition-all duration-150 focus-visible:ring-2 focus-visible:ring-accent/50 focus-visible:outline-none disabled:pointer-events-none disabled:opacity-40 ${
        checked
          ? "bg-accent hover:bg-accent-hover"
          : "bg-bg-tertiary hover:bg-bg-hover"
      }`}
    >
      <span
        className={`absolute left-0.5 top-0.5 h-4 w-4 rounded-full bg-white shadow-sm transition-transform duration-150 ${
          checked ? "translate-x-3.5" : "translate-x-0"
        }`}
      />
    </button>
  );
}
