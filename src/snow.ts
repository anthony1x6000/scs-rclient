/**
 * Seasonal snow: a purely decorative layer shown from November 20th through
 * December 31st. Everything here is deterministic and side-effect free so the
 * date gate and flake geometry can be unit tested without a browser.
 */

export interface Snowflake {
  /** Horizontal position across the viewport, in percent. */
  left: number;
  /** Static vertical position, in percent; used when motion is reduced. */
  top: number;
  /** Base size in pixels (dots use it as width/height, glyphs as font size). */
  size: number;
  /** Overall opacity, kept deliberately faint. */
  opacity: number;
  /** Seconds for one full fall. */
  duration: number;
  /** Seconds; negative so the flake starts mid-fall on mount. */
  delay: number;
  /** Lateral drift amplitude in pixels. */
  sway: number;
  /** Crystal glyph instead of a soft round dot. */
  glyph: boolean;
}

export const FLAKE_COUNT = 30;

/** True from November 20th through December 31st, inclusive. */
export function isSnowSeason(now: Date): boolean {
  const month = now.getMonth();
  const day = now.getDate();
  if (month === 10) return day >= 20;
  return month === 11;
}

function between(random: () => number, min: number, max: number): number {
  return min + random() * (max - min);
}

export function createSnowflakes(
  count = FLAKE_COUNT,
  random: () => number = Math.random,
): Snowflake[] {
  return Array.from({ length: count }, (_, index) => {
    const glyph = index % 7 === 0;
    const size = between(random, glyph ? 6 : 2, glyph ? 10 : 4);
    const duration = between(random, 22, 40);
    return {
      left: between(random, 0, 100),
      top: between(random, 0, 100),
      size,
      opacity: between(random, 0.06, 0.12),
      duration,
      delay: -between(random, 0, duration),
      sway: between(random, 8, 26),
      glyph,
    };
  });
}
