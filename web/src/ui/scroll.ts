/**
 * New scrollTop for the scroll host after a redraw: unchanged while the cursor row is on screen, otherwise the cursor
 * row at the bottom edge (0 when everything fits). `rowH` is the pixel height of one terminal row.
 */
export function cursorScrollTop(cursorRow: number, rowH: number, top: number, clientH: number): number {
  const bottom = (cursorRow + 1) * rowH;
  if (cursorRow * rowH >= top && bottom <= top + clientH) return top;
  return Math.max(0, bottom - clientH);
}
