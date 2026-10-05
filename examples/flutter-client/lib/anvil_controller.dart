import 'dart:async';

import 'package:anvil_link/anvil_link.dart';
import 'package:flutter/foundation.dart';

/// The app's state: a token, one socket, the announcements, the user's private channel and a presence room.
class AnvilController extends ChangeNotifier {
  AnvilController({required this.baseUrl, required this.appKey});

  final Uri baseUrl;
  final String appKey;

  String status = 'signed out';
  final List<String> events = [];

  /// User id → display name of the room's members.
  Map<String, String> members = {};
  int? userId;

  SmelteryApi? _api;
  AnvilSession? _session;
  String? _token;
  PresenceChannel? _room;
  PresenceRoster? _roster;
  final List<StreamSubscription<dynamic>> _subs = [];
  bool _disposed = false;

  bool get connected => _session != null;

  @override
  void notifyListeners() {
    if (!_disposed) super.notifyListeners();
  }

  void _log(String line) {
    events.insert(0, line);
    notifyListeners();
  }

  /// Signs in (`POST /api/tokens`), connects and subscribes the three channels.
  Future<void> connect({required String email, required String password, required int room}) async {
    status = 'signing in';
    notifyListeners();
    try {
      final api = _api = SmelteryApi(baseUrl);
      final token = _token = await api.issueToken(email: email, password: password, deviceName: 'flutter_client');
      userId = (await api.user(token))['id'] as int;
      final session = _session = AnvilSession(baseUrl: baseUrl, appKey: appKey, token: token);
      _subs.add(session.closes.listen((c) => _log('socket closed: ${c.code}')));
      // A revoked or signed-out token: the socket is closed with 4200, the client reconnects, and authorizing the
      // channels again answers 401. Back to the sign-in.
      _subs.add(session.unauthenticated.listen((_) => _endSession('signed out by the server: sign in again')));
      status = 'connecting';
      notifyListeners();
      await session.connect();
      status = 'connected as user $userId (socket ${session.socketId})';
      notifyListeners();

      final news = session.public('announcements');
      _subs.add(news.bindToAll().listen((e) => _log('announcements: ${e.name} ${e.data}')));
      _log('announcements: ${await session.subscribe(news)}');

      final mine = session.private('private-users.$userId');
      _subs.add(mine.bindToAll().listen((e) => _log('private-users.$userId: ${e.name} ${e.data}')));
      _log('private-users.$userId: ${await session.subscribe(mine)}');

      final presence = _room = session.presence('presence-chat.$room');
      final roster = _roster = PresenceRoster(presence);
      _subs.add(roster.changes.listen((_) => _refreshMembers()));
      _subs.add(presence.bindToAll().listen((e) => _log('presence-chat.$room: ${e.name} ${e.data} ${e.userId ?? ''}')));
      _log('presence-chat.$room: ${await session.subscribe(presence)}');
    } catch (e) {
      await _endSession('failed: $e');
    }
  }

  void _refreshMembers() {
    members = {
      for (final m in _roster?.members.entries ?? const <MapEntry<String, Map<String, dynamic>>>[])
        m.key: m.value['name']?.toString() ?? m.key,
    };
    notifyListeners();
  }

  /// Sends a client event to the room's other members.
  void whisper(String text) {
    final room = _room;
    final session = _session;
    if (room == null || session == null || text.isEmpty) return;
    session.whisper(room, 'message', {'text': text});
    _log('me (whisper): $text');
  }

  /// `POST /api/announce`: a broadcast on `announcements` (the example's server route).
  Future<void> announce(String text) async {
    final token = _token;
    if (token == null) return;
    final status = await _api!.postJson('/api/announce', token, {'message': text});
    _log('POST /api/announce: $status');
  }

  /// `DELETE /api/tokens/current`, then back to the sign-in.
  Future<void> signOut() async {
    final token = _token;
    if (token == null) return;
    final code = await _api!.revoke(token);
    _log('DELETE /api/tokens/current: $code');
    await _endSession('signed out');
  }

  Future<void> _endSession(String why) async {
    // Take everything first: a sign-out and the server's 401 can both end the session.
    final subs = [..._subs];
    _subs.clear();
    final roster = _roster;
    final session = _session;
    final api = _api;
    _roster = null;
    _session = null;
    _room = null;
    _token = null;
    _api = null;
    for (final s in subs) {
      await s.cancel();
    }
    await roster?.dispose();
    await session?.close();
    api?.close();
    members = {};
    status = why;
    notifyListeners();
  }

  Future<void> disconnect() => _endSession('signed out');

  @override
  void dispose() {
    _disposed = true;
    unawaited(_endSession('signed out'));
    super.dispose();
  }
}
