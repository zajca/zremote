import { type InputHTMLAttributes } from "react";

interface InputProps extends InputHTMLAttributes<HTMLInputElement> {
  label?: string;
}

export function Input({ label, id, className = "", ...props }: InputProps) {
  return (
    <div className="flex flex-col gap-1.5">
      {label && (
        <label htmlFor={id} className="text-xs font-medium text-text-secondary">
          {label}
        </label>
      )}
      <input
        id={id}
        className={`h-8 rounded-md border border-border bg-bg-tertiary px-3 text-sm text-text-primary transition-colors duration-150 placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none ${className}`}
        {...props}
      />
    </div>
  );
}
