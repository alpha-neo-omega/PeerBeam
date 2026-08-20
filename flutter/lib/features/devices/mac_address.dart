/// Hardware-address entry for waking a device.
///
/// # Why the UI parses this at all
///
/// A MAC has to be typed by hand: PeerBeam cannot discover one for a machine
/// that is switched off, which is the only kind of machine anybody wants to
/// wake. So the address arrives as whatever the user's other device printed —
/// colons on Linux, hyphens on Windows, dotted quads on network gear — and the
/// first chance to catch a typo is the field it is pasted into.
///
/// The accepted set is **exactly** the engine's (`aa:bb:cc:dd:ee:ff`,
/// `AA-BB-CC-DD-EE-FF`, `aabb.ccdd.eeff`), and deliberately not one shape
/// wider. A field that accepted `aabbccddeeff` would teach a form the CLI's
/// `wake set` refuses, and a UI rule the engine does not share is a rule
/// nobody re-checks — the failure would surface at the send, or worse, as a
/// silently reshaped address the user never agreed to.
library;

/// The colon form is what gets stored and shown, so the row, the CLI and the
/// engine all name the same address the same way.
final RegExp _colons = RegExp(r'^([0-9a-fA-F]{2}:){5}[0-9a-fA-F]{2}$');
final RegExp _hyphens = RegExp(r'^([0-9a-fA-F]{2}-){5}[0-9a-fA-F]{2}$');
final RegExp _dotted = RegExp(
  r'^[0-9a-fA-F]{4}\.[0-9a-fA-F]{4}\.[0-9a-fA-F]{4}$',
);
final RegExp _hex = RegExp(r'^[0-9a-fA-F]$');

/// The three shapes, for helper text that has to stay in step with the parser.
const String macShapes =
    'aa:bb:cc:dd:ee:ff, AA-BB-CC-DD-EE-FF or aabb.ccdd.eeff';

/// [input] as a canonical `aa:bb:cc:dd:ee:ff`, or the reason the engine would
/// refuse it. Exactly one of the two is non-null.
///
/// The refusal is a sentence a person can act on rather than "invalid": every
/// branch below names the specific thing that is wrong, because "that is not a
/// hardware address" in front of a 12-character string tells the user nothing
/// they did not already suspect.
({String? mac, String? refusal}) parseMac(String input) {
  final trimmed = input.trim();
  if (trimmed.isEmpty) {
    return (
      mac: null,
      refusal: 'Enter the device’s hardware address, like aa:bb:cc:dd:ee:ff.',
    );
  }
  if (_colons.hasMatch(trimmed) || _hyphens.hasMatch(trimmed)) {
    return (
      mac: _colonise(trimmed.replaceAll('-', '').replaceAll(':', '')),
      refusal: null,
    );
  }
  if (_dotted.hasMatch(trimmed)) {
    return (mac: _colonise(trimmed.replaceAll('.', '')), refusal: null);
  }
  return (mac: null, refusal: _refusal(trimmed));
}

/// Twelve hex digits → the lower-case colon form.
String _colonise(String digits) {
  final pairs = <String>[
    for (var i = 0; i < digits.length; i += 2) digits.substring(i, i + 2),
  ];
  return pairs.join(':').toLowerCase();
}

/// Why a string that matched none of the three shapes did not.
///
/// Ordered from the most specific fault to the least, so the message describes
/// what the user did rather than the last check to fail: a stray character is
/// worth naming even when the digit count is also wrong, since fixing the
/// character usually fixes both.
String _refusal(String value) {
  const separators = {':', '-', '.'};
  final stray = value
      .split('')
      .firstWhere(
        (c) => !_hex.hasMatch(c) && !separators.contains(c),
        orElse: () => '',
      );
  if (stray.isNotEmpty) {
    final shown = stray.trim().isEmpty ? 'a space' : '“$stray”';
    return 'Hardware addresses are hex digits (0-9, a-f) — $shown is not one.';
  }

  final used = separators.where(value.contains).toSet();
  final digits = value.split('').where(_hex.hasMatch).length;
  if (digits != 12) {
    return 'A hardware address has 12 hex digits; this has $digits.';
  }
  if (used.isEmpty) {
    return 'Group the digits in pairs: aa:bb:cc:dd:ee:ff.';
  }
  if (used.length > 1) {
    return 'Use one separator throughout, not ${used.map((s) => '“$s”').join(' and ')}.';
  }
  return switch (used.single) {
    '.' => 'The dotted form is three groups of four: aabb.ccdd.eeff.',
    '-' => 'The hyphen form is six pairs: AA-BB-CC-DD-EE-FF.',
    _ => 'The colon form is six pairs: aa:bb:cc:dd:ee:ff.',
  };
}
