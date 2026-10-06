import { useEffect, useRef, useState, type ReactNode } from "react";

const FOCUSABLE =
  'button:not([disabled]), a[href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * A modal for a Settings form. While `guarded`, Escape and a backdrop click
 * leave it open and report the attempt to its content instead, so something
 * shown only once is dismissed by its own button and never by accident.
 */
export function SettingsDialog({
  labelledBy,
  guarded = false,
  onClose,
  children,
}: {
  labelledBy: string;
  guarded?: boolean;
  onClose: () => void;
  /** Receives whether a guarded dismissal was just refused. */
  children: (refused: boolean) => ReactNode;
}) {
  const dialogRef = useRef<HTMLElement>(null);
  const [refused, setRefused] = useState(false);
  const dismiss = useRef(onClose);
  dismiss.current = guarded ? () => setRefused(true) : onClose;

  useEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialogRef.current?.focus();
    return () => opener?.focus();
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const dialog = dialogRef.current;
      if (!dialog) return;
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        dismiss.current();
        return;
      }
      if (event.key !== "Tab") return;
      const focusable = [...dialog.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
        (element) => element.getClientRects().length > 0,
      );
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const active = document.activeElement;
      if (!dialog.contains(active) || active === dialog) {
        event.preventDefault();
        (event.shiftKey ? last : first).focus();
      } else if (event.shiftKey && active === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && active === last) {
        event.preventDefault();
        first.focus();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  return (
    <div
      className="modal-backdrop set-modal-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) dismiss.current();
      }}
    >
      <section
        ref={dialogRef}
        className="modal set-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby={labelledBy}
        tabIndex={-1}
      >
        {children(guarded && refused)}
      </section>
    </div>
  );
}
