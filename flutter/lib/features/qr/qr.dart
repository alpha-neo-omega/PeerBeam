import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'package:qr_flutter/qr_flutter.dart';

import '../../app/theme.dart';

/// A peer address encoded in a QR. Wire format is a `peerbeam://` URI so it is
/// self-describing and rejects unrelated QR codes.
class QrPayload {
  final String name;
  final String host;
  final int port;
  const QrPayload({required this.name, required this.host, required this.port});

  /// `peerbeam://add?name=…&host=…&port=…`
  String encode() => Uri(
    scheme: 'peerbeam',
    host: 'add',
    queryParameters: {'name': name, 'host': host, 'port': '$port'},
  ).toString();

  /// Parse a scanned string, or null if it is not a valid PeerBeam address.
  static QrPayload? decode(String? raw) {
    if (raw == null) return null;
    final uri = Uri.tryParse(raw.trim());
    if (uri == null || uri.scheme != 'peerbeam') return null;
    final host = uri.queryParameters['host']?.trim() ?? '';
    final port = int.tryParse(uri.queryParameters['port'] ?? '') ?? 0;
    final name = uri.queryParameters['name']?.trim() ?? '';
    if (host.isEmpty || port <= 0 || port > 65535) return null;
    return QrPayload(name: name.isEmpty ? host : name, host: host, port: port);
  }
}

/// Show a saved device's address as a QR so another phone can scan it.
Future<void> showShareQrDialog(BuildContext context, QrPayload payload) {
  final scheme = Theme.of(context).colorScheme;
  return showDialog<void>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text('Share ${payload.name}'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Container(
            padding: const EdgeInsets.all(AppSpace.md),
            decoration: BoxDecoration(
              color: Colors.white,
              borderRadius: BorderRadius.circular(AppRadius.lg),
            ),
            child: QrImageView(
              data: payload.encode(),
              size: 220,
              backgroundColor: Colors.white,
              // Fixed dark modules render on the white quiet zone in both themes.
              eyeStyle: const QrEyeStyle(
                eyeShape: QrEyeShape.square,
                color: Color(0xFF111111),
              ),
              dataModuleStyle: const QrDataModuleStyle(
                dataModuleShape: QrDataModuleShape.square,
                color: Color(0xFF111111),
              ),
            ),
          ),
          const Gap(AppSpace.md),
          Text(
            '${payload.host}:${payload.port}',
            style: Theme.of(context).textTheme.bodyMedium?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
          const Gap(AppSpace.xs),
          Text(
            'Scan this from another device to add it.',
            textAlign: TextAlign.center,
            style: Theme.of(context).textTheme.bodySmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
      actions: [
        FilledButton(
          onPressed: () => Navigator.pop(context),
          child: const Text('Done'),
        ),
      ],
    ),
  );
}

/// Open the camera scanner; resolves to the first valid [QrPayload] scanned, or
/// null if the user backs out.
Future<QrPayload?> openQrScanner(BuildContext context) {
  return Navigator.of(context).push<QrPayload>(
    MaterialPageRoute(builder: (_) => const _QrScanScreen()),
  );
}

class _QrScanScreen extends StatefulWidget {
  const _QrScanScreen();

  @override
  State<_QrScanScreen> createState() => _QrScanScreenState();
}

class _QrScanScreenState extends State<_QrScanScreen> {
  final MobileScannerController _controller = MobileScannerController(
    detectionSpeed: DetectionSpeed.noDuplicates,
    formats: const [BarcodeFormat.qrCode],
  );
  bool _handled = false;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _onDetect(BarcodeCapture capture) {
    if (_handled) return;
    for (final barcode in capture.barcodes) {
      final payload = QrPayload.decode(barcode.rawValue);
      if (payload != null) {
        _handled = true;
        Navigator.of(context).pop(payload);
        return;
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Scan a device QR'),
        actions: [
          IconButton(
            tooltip: 'Toggle torch',
            icon: const Icon(Icons.flashlight_on_rounded),
            onPressed: () => _controller.toggleTorch(),
          ),
        ],
      ),
      body: Stack(
        fit: StackFit.expand,
        children: [
          MobileScanner(
            controller: _controller,
            onDetect: _onDetect,
            errorBuilder: (context, error) => _ScanError(error: error),
          ),
          // A simple reticle to aim with.
          IgnorePointer(
            child: Center(
              child: Container(
                width: 240,
                height: 240,
                decoration: BoxDecoration(
                  border: Border.all(color: Colors.white, width: 3),
                  borderRadius: BorderRadius.circular(AppRadius.lg),
                ),
              ),
            ),
          ),
          Positioned(
            left: 0,
            right: 0,
            bottom: AppSpace.xxl,
            child: Text(
              'Point at a PeerBeam QR',
              textAlign: TextAlign.center,
              style: Theme.of(context).textTheme.bodyMedium?.copyWith(
                color: Colors.white,
                fontWeight: FontWeight.w600,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _ScanError extends StatelessWidget {
  final MobileScannerException error;
  const _ScanError({required this.error});

  @override
  Widget build(BuildContext context) {
    final msg = switch (error.errorCode) {
      MobileScannerErrorCode.permissionDenied =>
        'Camera permission denied. Enable it in system settings to scan.',
      MobileScannerErrorCode.unsupported =>
        'Camera scanning is not supported on this device.',
      _ => 'Could not start the camera.',
    };
    return ColoredBox(
      color: Colors.black,
      child: Center(
        child: Padding(
          padding: const EdgeInsets.all(AppSpace.xxl),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Icon(
                Icons.no_photography_rounded,
                color: Colors.white70,
                size: AppIcons.xl,
              ),
              const Gap(AppSpace.md),
              Text(
                msg,
                textAlign: TextAlign.center,
                style: const TextStyle(color: Colors.white70),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
