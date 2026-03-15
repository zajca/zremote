import type { LucideIcon } from "lucide-react";
import { type ButtonHTMLAttributes } from "react";

interface IconButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  icon: LucideIcon;
  tooltip?: string;
}

export function IconButton({
  icon: Icon,
  tooltip,
  className = "",
  ...props
}: IconButtonProps) {
  return (
    <button
      className={`inline-flex h-7 w-7 items-center justify-center rounded-md text-text-secondary transition-all duration-150 hover:bg-bg-hover hover:text-text-primary focus-visible:ring-2 focus-visible:ring-border-hover focus-visible:outline-none disabled:pointer-events-none disabled:opacity-40 ${className}`}
      title={tooltip}
      {...props}
    >
      <Icon size={16} />
    </button>
  );
}
