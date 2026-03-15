type BadgeVariant = "online" | "offline" | "error" | "warning" | "creating";

interface BadgeProps {
  variant: BadgeVariant;
  children: React.ReactNode;
}

const variantStyles: Record<BadgeVariant, string> = {
  online: "bg-status-online/15 text-status-online",
  offline: "bg-status-offline/15 text-status-offline",
  error: "bg-status-error/15 text-status-error",
  warning: "bg-status-warning/15 text-status-warning",
  creating: "bg-accent/15 text-accent",
};

export function Badge({ variant, children }: BadgeProps) {
  return (
    <span
      className={`inline-flex items-center rounded px-1.5 py-0.5 text-xs font-medium ${variantStyles[variant]}`}
    >
      {children}
    </span>
  );
}
