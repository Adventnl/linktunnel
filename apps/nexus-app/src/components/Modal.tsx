import { useEffect, useRef, type ReactNode } from "react";

/** An accessible modal: labelled, closes on Escape, returns focus, keeps Tab inside. */
export function Modal({
  title,
  onClose,
  children,
  wide = false,
  closeLabel,
  danger = false,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  wide?: boolean;
  closeLabel: string;
  danger?: boolean;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    // The element that asked for focus, else the first control that is not the close button.
    const root = ref.current;
    const first =
      root?.querySelector<HTMLElement>("[data-autofocus]") ??
      root?.querySelector<HTMLElement>("input, select, textarea, button:not([data-close])") ??
      root?.querySelector<HTMLElement>("button");
    first?.focus();
    return () => previous?.focus?.();
  }, []);
  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      onClose();
    } else if (e.key === "Tab" && ref.current) {
      const f = [...ref.current.querySelectorAll<HTMLElement>("button, input, select, textarea, [tabindex]:not([tabindex='-1'])")].filter(
        (el) => !el.hasAttribute("disabled"),
      );
      const [a, b] = [f[0], f[f.length - 1]];
      if (a && b) {
        if (e.shiftKey && document.activeElement === a) {
          e.preventDefault();
          b.focus();
        } else if (!e.shiftKey && document.activeElement === b) {
          e.preventDefault();
          a.focus();
        }
      }
    }
  };
  return (
    <div className="fixed inset-0 z-40 flex items-start justify-center bg-black/50 p-4 pt-[12vh]" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onKeyDown={onKeyDown}
        className={`w-full ${wide ? "max-w-3xl" : "max-w-md"} rounded-xl border bg-panel text-fg shadow-[var(--shadow)] ${danger ? "border-danger" : "border-line"}`}
      >
        <div className="flex items-center justify-between gap-4 border-b border-line px-5 py-3">
          <h2 className="text-base font-semibold">{title}</h2>
          <button type="button" data-close aria-label={closeLabel} onClick={onClose} className="rounded p-1 text-muted hover:text-fg">
            ✕
          </button>
        </div>
        <div className="px-5 py-4">{children}</div>
      </div>
    </div>
  );
}

export function Button({
  children,
  onClick,
  kind = "secondary",
  disabled,
  autoFocus,
  title,
  type = "button",
}: {
  children: ReactNode;
  onClick?: () => void;
  kind?: "primary" | "secondary" | "danger" | "ghost";
  disabled?: boolean;
  autoFocus?: boolean;
  title?: string;
  type?: "button" | "submit";
}) {
  const base = "inline-flex items-center justify-center gap-1.5 rounded-lg px-3 py-1.5 text-sm font-medium transition-colors disabled:opacity-40 disabled:cursor-not-allowed";
  const styles = {
    primary: "bg-accent text-accent-fg hover:brightness-110",
    secondary: "border border-line bg-panel2 text-fg hover:border-accent",
    danger: "bg-danger text-danger-fg hover:brightness-110",
    ghost: "text-muted hover:text-fg",
  }[kind];
  return (
    <button type={type} title={title} data-autofocus={autoFocus ? "" : undefined} disabled={disabled} onClick={onClick} className={`${base} ${styles}`}>
      {children}
    </button>
  );
}
