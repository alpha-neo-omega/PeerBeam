import 'package:flutter/material.dart';

/// A presence indicator. Online dots pulse; offline are muted. The state is
/// exposed via [Semantics] (and callers pair it with text elsewhere), so it is
/// never conveyed by colour alone.
class StatusDot extends StatefulWidget {
  final bool online;
  final double size;
  const StatusDot({super.key, required this.online, this.size = 10});

  @override
  State<StatusDot> createState() => _StatusDotState();
}

class _StatusDotState extends State<StatusDot>
    with SingleTickerProviderStateMixin {
  late final AnimationController _c;

  @override
  void initState() {
    super.initState();
    // Created eagerly so dispose() never has to lazily build a ticker.
    _c = AnimationController(
      vsync: this,
      duration: const Duration(milliseconds: 1600),
    );
    if (widget.online) _c.repeat();
  }

  @override
  void didUpdateWidget(StatusDot old) {
    super.didUpdateWidget(old);
    if (widget.online && !_c.isAnimating) {
      _c.repeat();
    } else if (!widget.online && _c.isAnimating) {
      _c.stop();
    }
  }

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final color = widget.online ? const Color(0xFF22C55E) : scheme.outline;

    return Semantics(
      label: widget.online ? 'Online' : 'Offline',
      child: SizedBox(
        width: widget.size + 8,
        height: widget.size + 8,
        child: Center(
          child: Stack(
            alignment: Alignment.center,
            children: [
              if (widget.online)
                AnimatedBuilder(
                  animation: _c,
                  builder: (context, _) {
                    final t = _c.value;
                    return Container(
                      width: widget.size + t * 10,
                      height: widget.size + t * 10,
                      decoration: BoxDecoration(
                        shape: BoxShape.circle,
                        color: color.withValues(alpha: (1 - t) * 0.4),
                      ),
                    );
                  },
                ),
              Container(
                width: widget.size,
                height: widget.size,
                decoration: BoxDecoration(shape: BoxShape.circle, color: color),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
