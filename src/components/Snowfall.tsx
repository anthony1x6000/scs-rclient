import { useMemo, type CSSProperties } from "react";
import { createSnowflakes, isSnowSeason } from "../snow";
import "./snow.css";

/**
 * Decorative snowfall behind the app UI. Mounted once at the background layer
 * with pointer events off, and skipped entirely outside the season.
 */
export default function Snowfall() {
  const flakes = useMemo(() => createSnowflakes(), []);

  // Checked on mount: the season edge is a date away, so no timer is needed.
  if (!isSnowSeason(new Date())) return null;

  return (
    <div
      className="snowfall pointer-events-none fixed inset-0 z-0 overflow-hidden"
      aria-hidden="true"
    >
      {flakes.map((flake, index) => (
        <span
          key={index}
          className={`snow-flake ${flake.glyph ? "snow-flake-glyph" : "snow-flake-dot"}`}
          style={
            {
              left: `${flake.left}%`,
              "--flake-top": `${flake.top}%`,
              "--flake-size": `${flake.size}px`,
              "--flake-sway": `${flake.sway}px`,
              opacity: flake.opacity,
              animationDuration: `${flake.duration}s`,
              animationDelay: `${flake.delay}s`,
            } as CSSProperties
          }
        >
          {flake.glyph ? "❄" : null}
        </span>
      ))}
    </div>
  );
}
