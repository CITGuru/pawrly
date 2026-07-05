import type { ReactNode } from "react";

/** Label + control + hint/error wrapper for form fields. */
export function Field({
  label,
  required,
  help,
  error,
  htmlFor,
  children,
}: {
  label: string;
  required?: boolean;
  help?: ReactNode;
  error?: ReactNode;
  htmlFor?: string;
  children: ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <label htmlFor={htmlFor} className="block text-sm font-medium">
        {label}
        {required ? <span className="text-destructive"> *</span> : null}
      </label>
      {children}
      {error ? (
        <p className="text-destructive text-xs">{error}</p>
      ) : help ? (
        <p className="text-muted-foreground text-xs">{help}</p>
      ) : null}
    </div>
  );
}
