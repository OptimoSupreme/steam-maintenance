// Steam's React and Decky's component library are globals in the Steam UI, which
// the official rollup preset maps `react` and `@decky/ui` imports onto. Reading
// them here keeps the bundle free of both and type-checked against the real
// packages, which are only ever imported for their types.

export const React = (window as any).SP_REACT as typeof import('react');
export const DFL = (window as any).DFL as typeof import('@decky/ui');
