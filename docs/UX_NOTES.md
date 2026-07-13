# UX Notes

Practical notes for contributors touching the UI.

- **Snackbars:** use `friendlyError(e)` (from `lib/sdk/error_text.dart`) for any
  engine failure — never `'$e'`. Replace the current snackbar
  (`hideCurrentSnackBar()` first) to avoid stacking.
- **Motion:** wrap decorative durations in `AppMotion.duration(context, …)` so
  reduced-motion is honoured. Use `AppMotion.fast/medium/slow` + `curve`.
- **Colours:** semantic presence green is `AppColors.online`; everything else
  comes from `Theme.of(context).colorScheme`.
- **Semantics:** custom multi-widget rows (like the transfer card) get a single
  `Semantics(container:true, label: …)`; plain `ListTile`s are already announced.
- **State:** read via `AppScope.of(context)`; listen to the one repository a
  screen needs (`AnimatedBuilder(animation: state.<repo>)`).
- **Desktop-only** affordances gate behind `isDesktop`.
