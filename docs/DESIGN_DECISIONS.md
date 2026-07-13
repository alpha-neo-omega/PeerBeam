# Design Decisions (UI/UX)

- **One token source.** Motion (`AppMotion`), the presence colour
  (`AppColors.online`), and shape/radius (theme `CardTheme`/`listTileTheme`)
  live in one place so screens stay consistent and a change lands everywhere.
- **Reduced motion is first-class.** All decorative animation collapses to zero
  duration under the OS "reduce motion" setting — accessibility over flourish.
- **Errors never leak internals.** The engine returns typed codes; the UI maps
  them to friendly, actionable sentences (`error_text.dart`). Users never see
  `quic`, FFI, or exception text.
- **Granular reactivity.** State is split into per-domain `ChangeNotifier`
  repositories; each screen listens to only what it renders, avoiding tree-wide
  rebuilds (perceived performance) — no single god-provider.
- **No polling.** Everything reacts to engine events through the SDK stream.
- **Platform gating, not platform branching.** Desktop-only affordances
  (drag-and-drop, native pickers, keyboard tab-nav) are gated by `isDesktop`;
  mobile keeps its own flows. No platform-specific surprises in shared widgets.
- **Confirm destructive actions.** "Clear history" asks first; sends show a
  target picker rather than a silent action.

## Deliberate non-goals (M6)
No new features, no relayout without visual verification, no extra animation, no
startup-time or performance regressions.
