/**
 * Clamped index movement for a single-highlight list. `delta` is measured in
 * rows (1/-1 for arrow keys, a viewport's worth for PageUp/PageDown); entering
 * from -1 lands on the near edge in the direction of travel.
 */
export function moveHighlight(current: number, delta: number, total: number): number {
  if (total <= 0) return -1;
  if (current < 0) {
    if (delta > 0) return Math.min(delta - 1, total - 1);
    if (delta < 0) return Math.max(total + delta, 0);
    return 0;
  }
  return Math.max(0, Math.min(total - 1, current + delta));
}
