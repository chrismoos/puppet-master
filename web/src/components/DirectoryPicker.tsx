import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { fetchDirectory, filterEntries, listingPath, type DirEntry } from "../api/fs";

const FETCH_DEBOUNCE_MS = 160;
const MAX_SUGGESTIONS = 40;
const NO_ACTIVE = -1;

export function DirectoryPicker({
  value,
  onChange,
  placeholder,
  autoFocus,
  inputId,
  workerId,
}: {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  autoFocus?: boolean;
  inputId?: string;
  workerId?: bigint;
}) {
  const [entries, setEntries] = useState<DirEntry[]>([]);
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(NO_ACTIVE);
  const wrapRef = useRef<HTMLDivElement>(null);

  const listingKey = listingPath(value);

  useEffect(() => {
    const controller = new AbortController();
    const timer = setTimeout(() => {
      fetchDirectory(listingKey, workerId, controller.signal)
        .then((listing) => setEntries(listing.entries))
        .catch(() => {
          /* Unreachable dir or aborted fetch leaves the last suggestions in place. */
        });
    }, FETCH_DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [listingKey, workerId]);

  const suggestions = useMemo(
    () => filterEntries(entries, value).slice(0, MAX_SUGGESTIONS),
    [entries, value],
  );

  useEffect(() => {
    const onDocClick = (e: MouseEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, []);

  const complete = (entry: DirEntry) => {
    onChange(`${entry.path}/`);
    setActive(NO_ACTIVE);
    setOpen(true);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (!open || suggestions.length === 0) {
      if (e.key === "ArrowDown") setOpen(true);
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => (i + 1) % suggestions.length);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => (i <= 0 ? suggestions.length - 1 : i - 1));
    } else if (e.key === "Enter" && active >= 0) {
      e.preventDefault();
      complete(suggestions[active]);
    } else if (e.key === "Escape") {
      setOpen(false);
      setActive(NO_ACTIVE);
    }
  };

  return (
    <div className="dirpicker" ref={wrapRef}>
      <input
        id={inputId}
        type="text"
        value={value}
        autoFocus={autoFocus}
        placeholder={placeholder}
        spellCheck={false}
        autoComplete="off"
        onChange={(e) => {
          onChange(e.target.value);
          setOpen(true);
          setActive(NO_ACTIVE);
        }}
        onFocus={() => setOpen(true)}
        onKeyDown={onKeyDown}
      />
      {open && suggestions.length > 0 && (
        <ul className="dirpicker-list" role="listbox">
          {suggestions.map((entry, i) => (
            <li key={entry.path}>
              <button
                type="button"
                role="option"
                aria-selected={i === active}
                className={`dirpicker-option ${i === active ? "is-active" : ""}`}
                onMouseEnter={() => setActive(i)}
                onClick={() => complete(entry)}
              >
                {entry.name}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
