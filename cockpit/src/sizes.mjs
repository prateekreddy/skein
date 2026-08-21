// Sizes, in the units a person reads.
//
// Two of them, because the server sends bytes for memory and mebibytes for disk, and a single
// formatter taking "a number" is one that eventually gets handed the wrong one.

// Bytes.
export const fmtGB = b =>
  b >= 1073741824 ? `${(b / 1073741824).toFixed(1)}G` : `${Math.round(b / 1048576)}M`;

// Mebibytes.
export const fmtGb = mb => (mb >= 1024 ? `${(mb / 1024).toFixed(1)}G` : `${mb}M`);
