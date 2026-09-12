/**
 * The one window size, shared by the runner and the capture script.
 *
 * Wide enough to be past the shell's `md` breakpoint, so "is the sidebar inline
 * or in a drawer" is decided here rather than by whatever viewport a runner
 * defaults to. And the same on both sides of the fidelity comparison — a
 * reference measured at another width is not a reference.
 *
 * It lives under `src/` rather than in `playwright.config.ts` because the
 * config is outside every package's `src`, and a spec reaching up to it is the
 * deep relative import the repo bans everywhere else for good reasons.
 */
export const VIEWPORT: { readonly width: number; readonly height: number } = {
  width: 1440,
  height: 900,
};

/**
 * A phone, for the specs that assert the layout survives one.
 *
 * Not a second default — the runner still opens at `VIEWPORT`, and the fidelity
 * references are still measured there. This is the size a reflow assertion
 * resizes *to*, and it is the narrow end of what a phone actually is rather
 * than the narrowest number anyone could name: at 375 the shell is in its
 * drawer, the settings grid is one column, and a row that fits here fits every
 * handset above it.
 */
export const NARROW_VIEWPORT: {
  readonly width: number;
  readonly height: number;
} = {
  width: 375,
  height: 812,
};
