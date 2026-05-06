import React, { useCallback, useEffect, useState } from "react";

// Three-pane horizontal splitter with draggable dividers.
//
// Layout: [leftW px] [1px divider] [flex middle] [1px divider] [rightW px].
// The divider's visible width is 1px (matches Explore's pane separators) but
// it has a 7px transparent hit area so users can grab it without precision.
//
// Widths are persisted to localStorage under `${storageKey}` so adjustments
// survive reloads and tab switches.

interface Props {
  storageKey: string;
  leftDefaultPx?: number;
  rightDefaultPx?: number;
  leftMinPx?: number;
  leftMaxPx?: number;
  rightMinPx?: number;
  rightMaxPx?: number;
  className?: string;
  /** Three children: left, middle, right. */
  children: React.ReactNode;
}

export function HSplit({
  storageKey,
  leftDefaultPx = 360,
  rightDefaultPx = 360,
  leftMinPx = 240,
  leftMaxPx = 800,
  rightMinPx = 240,
  rightMaxPx = 800,
  className = "",
  children,
}: Props) {
  const [leftW, setLeftW] = useState<number>(() =>
    loadStoredWidth(`${storageKey}-left`, leftDefaultPx),
  );
  const [rightW, setRightW] = useState<number>(() =>
    loadStoredWidth(`${storageKey}-right`, rightDefaultPx),
  );

  useEffect(() => {
    try {
      localStorage.setItem(`${storageKey}-left`, String(leftW));
    } catch {
      /* localStorage may be disabled */
    }
  }, [storageKey, leftW]);
  useEffect(() => {
    try {
      localStorage.setItem(`${storageKey}-right`, String(rightW));
    } catch {
      /* localStorage may be disabled */
    }
  }, [storageKey, rightW]);

  const onDragLeft = useCallback(
    (dx: number) => {
      setLeftW((w) => clamp(w + dx, leftMinPx, leftMaxPx));
    },
    [leftMinPx, leftMaxPx],
  );
  const onDragRight = useCallback(
    (dx: number) => {
      // Right pane shrinks when divider moves right (positive dx).
      setRightW((w) => clamp(w - dx, rightMinPx, rightMaxPx));
    },
    [rightMinPx, rightMaxPx],
  );

  const childArr = React.Children.toArray(children);

  return (
    <div
      className={"hsplit " + className}
      style={{
        display: "grid",
        gridTemplateColumns: `${leftW}px 1px minmax(0, 1fr) 1px ${rightW}px`,
      }}
    >
      <div className="hsplit-pane">{childArr[0]}</div>
      <Divider onDrag={onDragLeft} />
      <div className="hsplit-pane">{childArr[1]}</div>
      <Divider onDrag={onDragRight} />
      <div className="hsplit-pane">{childArr[2]}</div>
    </div>
  );
}

function Divider({ onDrag }: { onDrag: (dx: number) => void }) {
  const onMouseDown = (e: React.MouseEvent) => {
    e.preventDefault();
    let lastX = e.clientX;
    const onMove = (ev: MouseEvent) => {
      const dx = ev.clientX - lastX;
      lastX = ev.clientX;
      if (dx !== 0) onDrag(dx);
    };
    const onUp = () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  };
  return <div className="hsplit-divider" onMouseDown={onMouseDown} role="separator" />;
}

function loadStoredWidth(key: string, fallback: number): number {
  try {
    const v = localStorage.getItem(key);
    if (v !== null) {
      const n = parseInt(v, 10);
      if (!Number.isNaN(n) && n > 0) return n;
    }
  } catch {
    /* localStorage may be disabled */
  }
  return fallback;
}

function clamp(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, v));
}
