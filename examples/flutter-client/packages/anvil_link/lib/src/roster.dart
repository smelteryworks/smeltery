import 'dart:async';

import 'package:dart_pusher_channels/dart_pusher_channels.dart';

/// The members of a presence channel with their `user_info`.
///
/// `dart_pusher_channels` keeps only the ids of the member list that comes with the subscription
/// (`PresenceChannel.state.members` has `info == null` for them); the full list is in the data of the
/// `pusher:subscription_succeeded` event (`{"presence": {"ids", "hash", "count"}}`), and `member_added` carries
/// `{"user_id", "user_info"}`. This roster follows those three events.
class PresenceRoster {
  PresenceRoster(PresenceChannel channel) {
    _subs.addAll([
      channel.whenSubscriptionSucceeded().listen((e) {
        final presence = e.tryGetDataAsMap()?['presence'];
        final hash = presence is Map ? presence['hash'] : null;
        members
          ..clear()
          ..addAll({
            if (hash is Map)
              for (final entry in hash.entries) '${entry.key}': _info(entry.value),
          });
        _changes.add(null);
      }),
      channel.whenMemberAdded().listen((e) {
        final data = e.tryGetDataAsMap();
        final id = data?['user_id']?.toString();
        if (id == null) return;
        members[id] = _info(data?['user_info']);
        _changes.add(null);
      }),
      channel.whenMemberRemoved().listen((e) {
        final id = e.tryGetDataAsMap()?['user_id']?.toString();
        if (id == null) return;
        members.remove(id);
        _changes.add(null);
      }),
    ]);
  }

  /// User id → `user_info`.
  final Map<String, Map<String, dynamic>> members = {};
  final List<StreamSubscription<dynamic>> _subs = [];
  final StreamController<void> _changes = StreamController.broadcast();

  /// Fires after each change of [members].
  Stream<void> get changes => _changes.stream;

  static Map<String, dynamic> _info(Object? value) =>
      value is Map ? value.map((k, v) => MapEntry('$k', v)) : <String, dynamic>{};

  Future<void> dispose() async {
    for (final s in _subs) {
      await s.cancel();
    }
    await _changes.close();
  }
}
