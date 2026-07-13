import 'package:flutter/material.dart';

/// PeerBeam Material 3 theme — a single seed drives a full, tonal light/dark
/// palette. Both brightnesses share component shaping so the app reads as one
/// system regardless of mode.
class PeerBeamTheme {
  PeerBeamTheme._();

  /// Brand seed (indigo). All roles are derived tonally from this.
  static const Color seed = Color(0xFF6366F1);

  static ThemeData light() => _build(Brightness.light);
  static ThemeData dark() => _build(Brightness.dark);

  static ThemeData _build(Brightness brightness) {
    final scheme = ColorScheme.fromSeed(
      seedColor: seed,
      brightness: brightness,
    );
    final base = ThemeData(colorScheme: scheme, useMaterial3: true);

    return base.copyWith(
      scaffoldBackgroundColor: scheme.surface,
      appBarTheme: AppBarTheme(
        centerTitle: false,
        scrolledUnderElevation: 2,
        backgroundColor: scheme.surface,
        foregroundColor: scheme.onSurface,
        titleTextStyle: base.textTheme.titleLarge?.copyWith(
          fontWeight: FontWeight.w700,
        ),
      ),
      cardTheme: CardThemeData(
        elevation: 0,
        clipBehavior: Clip.antiAlias,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(20),
          side: BorderSide(color: scheme.outlineVariant.withValues(alpha: 0.6)),
        ),
        color: scheme.surfaceContainerLow,
      ),
      navigationBarTheme: NavigationBarThemeData(
        elevation: 2,
        backgroundColor: scheme.surfaceContainer,
        indicatorShape: const StadiumBorder(),
        labelBehavior: NavigationDestinationLabelBehavior.onlyShowSelected,
      ),
      navigationRailTheme: NavigationRailThemeData(
        backgroundColor: scheme.surface,
        indicatorShape: const StadiumBorder(),
        useIndicator: true,
      ),
      filledButtonTheme: FilledButtonThemeData(
        style: FilledButton.styleFrom(
          padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 14),
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(14),
          ),
        ),
      ),
      listTileTheme: const ListTileThemeData(
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.all(Radius.circular(16)),
        ),
      ),
      snackBarTheme: SnackBarThemeData(
        behavior: SnackBarBehavior.floating,
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(12)),
      ),
    );
  }
}

/// Shared motion tokens so animations feel consistent and "native".
class AppMotion {
  AppMotion._();
  static const Duration fast = Duration(milliseconds: 150);
  static const Duration medium = Duration(milliseconds: 260);
  static const Duration slow = Duration(milliseconds: 420);
  static const Curve curve = Curves.easeOutCubic;
  static const Curve emphasized = Curves.easeInOutCubicEmphasized;

  /// Respect the OS "reduce motion" accessibility setting: `false` when the
  /// platform asks animations to be disabled. Decorative motion should be
  /// skipped when this is `false`.
  static bool enabled(BuildContext context) =>
      !MediaQuery.of(context).disableAnimations;

  /// A duration that collapses to zero when reduced motion is requested, so
  /// implicit animations resolve instantly instead of moving.
  static Duration duration(BuildContext context, Duration normal) =>
      enabled(context) ? normal : Duration.zero;
}

/// Semantic colours not carried by the [ColorScheme] (kept consistent in one
/// place). Presence green is the same in light and dark.
class AppColors {
  AppColors._();

  /// Online / success presence indicator.
  static const Color online = Color(0xFF22C55E);
}

/// Layout breakpoints (Material 3 window size classes, simplified).
class Breakpoints {
  Breakpoints._();
  static const double compact = 600; // phone
  static const double medium = 1000; // tablet / small desktop
  static const double contentMaxWidth = 900; // readable line length cap
}
