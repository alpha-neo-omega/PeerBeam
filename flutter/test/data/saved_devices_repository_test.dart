// Tests for SavedDevicesRepository's defensive load(): a single corrupt
// saved-device entry must not discard the rest of the list.

import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:peerbeam/data/saved_devices_repository.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  group('SavedDevicesRepository.load', () {
    test('skips a corrupt entry but keeps the valid ones', () async {
      SharedPreferences.setMockInitialValues({
        'saved_devices_v1': jsonEncode([
          {'id': '1', 'name': 'Desk', 'host': '192.168.1.10', 'port': 49600},
          // Wrong field type (host is a number, not a String) — throws in
          // SavedDevice.fromJson.
          {'id': '2', 'name': 'Bad', 'host': 42, 'port': 49600},
          // Not even a Map — throws on the cast.
          'not-a-device',
          {'id': '3', 'name': 'Server', 'host': '192.168.1.11', 'port': 22},
        ]),
      });

      final repo = SavedDevicesRepository();
      await repo.load();

      expect(repo.devices.map((d) => d.id), ['1', '3']);
      expect(repo.devices.map((d) => d.name), ['Desk', 'Server']);
    });

    test('a fully-corrupt store loads to empty instead of crashing', () async {
      SharedPreferences.setMockInitialValues({
        'saved_devices_v1': 'not json at all {{{',
      });

      final repo = SavedDevicesRepository();
      await repo.load();

      expect(repo.devices, isEmpty);
    });

    test('no stored value loads to an empty list', () async {
      SharedPreferences.setMockInitialValues({});

      final repo = SavedDevicesRepository();
      await repo.load();

      expect(repo.devices, isEmpty);
    });
  });
}
