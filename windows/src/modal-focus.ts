export function focusBoundaryIndex(length: number, current: number, backwards: boolean): number | null {
  if (length === 0) return null;
  if (current < 0) return backwards ? length - 1 : 0;
  if (backwards && current === 0) return length - 1;
  if (!backwards && current === length - 1) return 0;
  return null;
}
