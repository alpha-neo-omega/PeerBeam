import 'package:flutter/material.dart';

/// PeerBeam Material 3 theme — one seed drives a full tonal light/dark palette,
/// and a single set of design tokens (spacing, radius, elevation, motion) keeps
/// every screen visually consistent. Both brightnesses share component shaping
/// so the app reads as one system regardless of mode.
class PeerBeamTheme {
  PeerBeamTheme._();

  /// Brand seed — the Material 3 baseline color, for the stock Material look
  /// (LocalSend-style). All roles are derived tonally from this.
  static const Color seed = Color(0xFF6750A4);

  static ThemeData light() => _build(Brightness.light);
  static ThemeData dark() => _build(Brightness.dark);

  static ThemeData _build(Brightness brightness) {
    final scheme = ColorScheme.fromSeed(
      seedColor: seed,
      brightness: brightness,
    );
    final base = ThemeData(
      colorScheme: scheme,
      useMaterial3: true,
      // Bundled Google Sans Flex (see pubspec fonts) — the Google Sans look,
      // shipped offline under the OFL.
      fontFamily: 'Google Sans Flex',
    );
    final text = _typography(base.textTheme);

    return base.copyWith(
      scaffoldBackgroundColor: scheme.surface,
      textTheme: text,
      splashFactory: InkSparkle.splashFactory,
      visualDensity: VisualDensity.standard,

      appBarTheme: AppBarTheme(
        centerTitle: false,
        scrolledUnderElevation: 3,
        backgroundColor: scheme.surface,
        surfaceTintColor: scheme.surfaceTint,
        foregroundColor: scheme.onSurface,
        titleTextStyle: text.titleLarge?.copyWith(fontWeight: FontWeight.w700),
      ),

      // Flat, borderless, tonally-tinted cards — the soft stock-Material look.
      cardTheme: CardThemeData(
        elevation: AppElevation.level0,
        clipBehavior: Clip.antiAlias,
        margin: EdgeInsets.zero,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(AppRadius.xl),
        ),
        color: scheme.surfaceContainerLow,
      ),

      navigationBarTheme: NavigationBarThemeData(
        elevation: AppElevation.level2,
        height: 68,
        backgroundColor: scheme.surfaceContainer,
        surfaceTintColor: Colors.transparent,
        indicatorShape: const StadiumBorder(),
        indicatorColor: scheme.secondaryContainer,
        labelBehavior: NavigationDestinationLabelBehavior.onlyShowSelected,
        labelTextStyle: WidgetStateProperty.resolveWith(
          (s) => text.labelMedium?.copyWith(
            fontWeight: s.contains(WidgetState.selected)
                ? FontWeight.w700
                : FontWeight.w500,
          ),
        ),
      ),

      navigationRailTheme: NavigationRailThemeData(
        backgroundColor: scheme.surface,
        indicatorShape: const StadiumBorder(),
        indicatorColor: scheme.secondaryContainer,
        useIndicator: true,
        selectedLabelTextStyle: text.labelMedium?.copyWith(
          fontWeight: FontWeight.w700,
          color: scheme.onSurface,
        ),
        unselectedLabelTextStyle: text.labelMedium?.copyWith(
          color: scheme.onSurfaceVariant,
        ),
      ),

      filledButtonTheme: FilledButtonThemeData(style: _btn(scheme)),
      elevatedButtonTheme: ElevatedButtonThemeData(style: _btn(scheme)),
      outlinedButtonTheme: OutlinedButtonThemeData(style: _btn(scheme)),
      textButtonTheme: TextButtonThemeData(
        style: TextButton.styleFrom(
          padding: const EdgeInsets.symmetric(
            horizontal: AppSpace.md,
            vertical: AppSpace.sm,
          ),
          shape: const StadiumBorder(),
          textStyle: text.labelLarge?.copyWith(fontWeight: FontWeight.w600),
        ),
      ),

      // Stock segmented (stadium) shape.
      segmentedButtonTheme: const SegmentedButtonThemeData(
        style: ButtonStyle(shape: WidgetStatePropertyAll(StadiumBorder())),
      ),

      chipTheme: ChipThemeData(
        shape: const StadiumBorder(),
        side: BorderSide.none,
        labelStyle: text.labelMedium?.copyWith(fontWeight: FontWeight.w600),
        backgroundColor: scheme.surfaceContainerHighest,
        padding: const EdgeInsets.symmetric(
          horizontal: AppSpace.sm,
          vertical: 4,
        ),
      ),

      listTileTheme: ListTileThemeData(
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(AppRadius.lg),
        ),
        iconColor: scheme.onSurfaceVariant,
      ),

      inputDecorationTheme: InputDecorationTheme(
        filled: true,
        fillColor: scheme.surfaceContainerHighest.withValues(alpha: 0.5),
        contentPadding: const EdgeInsets.symmetric(
          horizontal: AppSpace.md,
          vertical: AppSpace.sm + 2,
        ),
        border: OutlineInputBorder(
          borderRadius: BorderRadius.circular(AppRadius.md),
          borderSide: BorderSide.none,
        ),
        enabledBorder: OutlineInputBorder(
          borderRadius: BorderRadius.circular(AppRadius.md),
          borderSide: BorderSide.none,
        ),
        focusedBorder: OutlineInputBorder(
          borderRadius: BorderRadius.circular(AppRadius.md),
          borderSide: BorderSide(color: scheme.primary, width: 2),
        ),
      ),

      dialogTheme: DialogThemeData(
        elevation: AppElevation.level3,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(AppRadius.xxl),
        ),
        backgroundColor: scheme.surfaceContainerHigh,
      ),

      bottomSheetTheme: BottomSheetThemeData(
        backgroundColor: scheme.surfaceContainerLow,
        surfaceTintColor: Colors.transparent,
        shape: const RoundedRectangleBorder(
          borderRadius: BorderRadius.vertical(top: Radius.circular(AppRadius.xxl)),
        ),
        showDragHandle: true,
      ),

      snackBarTheme: SnackBarThemeData(
        behavior: SnackBarBehavior.floating,
        elevation: AppElevation.level3,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(AppRadius.md),
        ),
      ),

      dividerTheme: DividerThemeData(
        thickness: 1,
        space: 1,
        color: scheme.outlineVariant.withValues(alpha: 0.5),
      ),

      tooltipTheme: TooltipThemeData(
        decoration: BoxDecoration(
          color: scheme.inverseSurface,
          borderRadius: BorderRadius.circular(AppRadius.sm),
        ),
        textStyle: text.labelMedium?.copyWith(color: scheme.onInverseSurface),
      ),
    );
  }

  /// Shared button shape/padding for filled/elevated/outlined variants.
  /// Stadium (pill) — the stock Material 3 button silhouette.
  static ButtonStyle _btn(ColorScheme scheme) => const ButtonStyle(
    padding: WidgetStatePropertyAll(
      EdgeInsets.symmetric(horizontal: AppSpace.lg, vertical: AppSpace.sm + 2),
    ),
    shape: WidgetStatePropertyAll(StadiumBorder()),
    textStyle: WidgetStatePropertyAll(
      TextStyle(fontWeight: FontWeight.w600, fontSize: 14, letterSpacing: 0.1),
    ),
  );

  /// Typography refinements: tighter tracking + confident weights on display /
  /// headline / title roles, keeping M3 sizing so layouts stay stable.
  static TextTheme _typography(TextTheme t) => t.copyWith(
    headlineLarge: t.headlineLarge?.copyWith(
      fontWeight: FontWeight.w700,
      letterSpacing: -0.5,
    ),
    headlineMedium: t.headlineMedium?.copyWith(
      fontWeight: FontWeight.w700,
      letterSpacing: -0.25,
    ),
    headlineSmall: t.headlineSmall?.copyWith(fontWeight: FontWeight.w700),
    titleLarge: t.titleLarge?.copyWith(
      fontWeight: FontWeight.w700,
      letterSpacing: -0.2,
    ),
    titleMedium: t.titleMedium?.copyWith(fontWeight: FontWeight.w600),
    titleSmall: t.titleSmall?.copyWith(fontWeight: FontWeight.w600),
    labelLarge: t.labelLarge?.copyWith(fontWeight: FontWeight.w600),
  );
}

/// Spacing scale (8-pt grid, with a 4 half-step). Use everywhere instead of
/// ad-hoc pixel values so rhythm stays consistent.
class AppSpace {
  AppSpace._();
  static const double xxs = 4;
  static const double xs = 8;
  static const double sm = 12;
  static const double md = 16;
  static const double lg = 20;
  static const double xl = 24;
  static const double xxl = 32;
  static const double xxxl = 40;
}

/// Corner-radius scale.
class AppRadius {
  AppRadius._();
  static const double sm = 8;
  static const double md = 12;
  static const double lg = 16;
  static const double xl = 20;
  static const double xxl = 28;
  static const double full = 999;
}

/// Elevation steps (Material 3 tonal levels).
class AppElevation {
  AppElevation._();
  static const double level0 = 0;
  static const double level1 = 1;
  static const double level2 = 3;
  static const double level3 = 6;
}

/// Standard icon sizes.
class AppIcons {
  AppIcons._();
  static const double sm = 18;
  static const double md = 22;
  static const double lg = 28;
  static const double xl = 40;
}

/// A square spacer that adds [size] along whichever axis its parent lays out —
/// `Gap(AppSpace.md)` works in both a `Column` and a `Row`.
class Gap extends StatelessWidget {
  final double size;
  const Gap(this.size, {super.key});
  @override
  Widget build(BuildContext context) => SizedBox(width: size, height: size);
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
/// place). These read the same in light and dark.
class AppColors {
  AppColors._();

  /// Online / success presence indicator.
  static const Color online = Color(0xFF22C55E);

  /// Success (completed transfer).
  static const Color success = Color(0xFF22C55E);

  /// Warning / attention.
  static const Color warning = Color(0xFFF59E0B);
}

/// Layout breakpoints (Material 3 window size classes, simplified).
class Breakpoints {
  Breakpoints._();
  static const double compact = 600; // phone
  static const double medium = 1000; // tablet / small desktop
  static const double contentMaxWidth = 900; // readable line length cap
}
