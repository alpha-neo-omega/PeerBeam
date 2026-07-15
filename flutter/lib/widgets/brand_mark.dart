import 'package:flutter/material.dart';

import '../app/theme.dart';

/// The PeerBeam brand mark (bundled logo asset). Transparent PNG, so it sits
/// on any surface without a tile behind it.
class PeerBeamMark extends StatelessWidget {
  final double size;
  const PeerBeamMark({super.key, this.size = 34});

  @override
  Widget build(BuildContext context) {
    return Image.asset(
      'assets/brand/peerbeam-mark.png',
      width: size,
      height: size,
      fit: BoxFit.contain,
      semanticLabel: 'PeerBeam',
      filterQuality: FilterQuality.medium,
    );
  }
}

/// The brand mark beside the wordmark — used in the nav rail leading.
class BrandLockup extends StatelessWidget {
  final bool showWordmark;
  const BrandLockup({super.key, this.showWordmark = true});

  @override
  Widget build(BuildContext context) {
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        const PeerBeamMark(),
        if (showWordmark) ...[
          const Gap(AppSpace.xs),
          Text(
            'PeerBeam',
            style: Theme.of(
              context,
            ).textTheme.titleMedium?.copyWith(fontWeight: FontWeight.w700),
          ),
        ],
      ],
    );
  }
}
