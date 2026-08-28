// Dense when the fleet is large, comfortable when it is small — §11.7, same components either way.
//
// It is a function of how many rows there are, not of the window: a fleet of three is comfortable on
// a laptop and on a wall display, and a fleet of forty is a list you scan on both. Tying it to
// viewport width would make the same fleet read differently on two screens, which is the opposite of
// an identity learned once.

export const COMFORTABLE = "comfortable";
export const DENSE = "dense";

// Where it switches. Twelve rows is about where a page stops being read and starts being scanned —
// and it is one number in one place so that "adaptive" cannot come to mean two components
// disagreeing about what a large fleet is.
export const MANY = 12;

export const densityFor = rows => ((rows || 0) >= MANY ? DENSE : COMFORTABLE);
