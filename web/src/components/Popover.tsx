import { useEffect, useRef, type ReactNode } from "react";

/**
 * A trigger button with a floating menu that closes on outside click or
 * Escape. The parent owns `open` so it can keep one menu open at a time.
 */
export function Popover({
  open,
  onToggle,
  onClose,
  triggerLabel,
  triggerTitle,
  triggerClassName,
  align = "right",
  children,
}: {
  open: boolean;
  onToggle: () => void;
  onClose: () => void;
  triggerLabel: string;
  triggerTitle: string;
  triggerClassName?: string;
  align?: "left" | "right";
  children: ReactNode;
}) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) closeRef.current();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        closeRef.current();
        requestAnimationFrame(() => triggerRef.current?.focus());
      }
    };
    document.addEventListener("mousedown", onDown);
    // Capture Escape before terminal widgets can consume it. A live xterm may
    // reclaim focus while a sidebar menu is open during activity rerenders.
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey, true);
    };
  }, [open]);

  return (
    <div className="popover" ref={wrapRef}>
      <button
        ref={triggerRef}
        type="button"
        className={`popover-trigger ${triggerClassName ?? ""}`}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={triggerTitle}
        title={triggerTitle}
        onClick={onToggle}
      >
        <span aria-hidden="true">{triggerLabel}</span>
      </button>
      {open && (
        <div className={`popover-menu popover-menu-${align}`} role="menu">
          {children}
        </div>
      )}
    </div>
  );
}
