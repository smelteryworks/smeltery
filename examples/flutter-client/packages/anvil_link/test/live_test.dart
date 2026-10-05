// Live checks against a running Smeltery app with Anvil + Hallmark (README.md: the server setup).
// Settings come from the environment:
//   SMELTERY_URL        the app's origin (default http://127.0.0.1:8000)
//   ANVIL_APP_KEY       the app's ANVIL_APP_KEY (default smeltery-example-key)
//   SMELTERY_EMAIL, SMELTERY_EMAIL2, SMELTERY_PASSWORD
//                       two users of the server app (defaults: the seeder of README.md)
// Run: dart test test/live_test.dart  (tagged `live`; skipped when the app does not answer /api/health)
@Tags(['live'])
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:anvil_link/anvil_link.dart';
import 'package:http/http.dart' as http;
import 'package:test/test.dart';

final env = Platform.environment;
final base = Uri.parse(env['SMELTERY_URL'] ?? 'http://127.0.0.1:8000');
final appKey = env['ANVIL_APP_KEY'] ?? 'smeltery-example-key';
final emailA = env['SMELTERY_EMAIL'] ?? 'demo@example.com';
final emailB = env['SMELTERY_EMAIL2'] ?? 'second@example.com';
final password = env['SMELTERY_PASSWORD'] ?? 'password';

/// The first event named [name] on [stream] within [timeout], or null.
Future<T?> firstOrNull<T>(Stream<T> stream, {Duration timeout = const Duration(seconds: 5)}) async {
  try {
    return await stream.first.timeout(timeout);
  } on TimeoutException {
    return null;
  }
}

Future<bool> serverUp() async {
  try {
    final r = await http.get(base.resolve('/api/health')).timeout(const Duration(seconds: 3));
    return r.statusCode == 200;
  } catch (_) {
    return false;
  }
}

void main() async {
  final up = await serverUp();
  final skip = up ? null : 'no Smeltery app answers at $base/api/health';

  late SmelteryApi api;
  late String tokenA;
  late String tokenB;
  late int idA;
  late int idB;
  late AnvilSession a;
  AnvilSession? b;
  late PresenceChannel roomA;
  late PresenceRoster rosterA;

  setUpAll(() async {
    if (!up) return;
    api = SmelteryApi(base);
  });

  tearDownAll(() async {
    if (!up) return;
    await rosterA.dispose();
    await b?.close();
    await a.close();
    api.close();
    final dump = env['WIRE_DUMP'];
    if (dump != null) {
      File(dump).writeAsStringSync([
        '# client A',
        ...a.wire.map((r) => r.toString()),
        '# client B',
        ...?b?.wire.map((r) => r.toString()),
      ].join('\n'));
    }
  });

  group('Anvil from Dart', skip: skip, () {
    test('POST /api/tokens issues a bearer token for each user; GET /api/user names them', () async {
      tokenA = await api.issueToken(email: emailA, password: password, deviceName: 'dart live A');
      tokenB = await api.issueToken(email: emailB, password: password, deviceName: 'dart live B');
      expect(tokenA, matches(RegExp(r'^smt_[0-9a-f]{64}$')));
      expect(tokenB, isNot(tokenA));
      idA = (await api.user(tokenA))['id'] as int;
      idB = (await api.user(tokenB))['id'] as int;
      expect(idA, isNot(idB));
    });

    test('the socket connects with protocol 7 and gets a socket id', () async {
      a = AnvilSession(baseUrl: base, appKey: appKey, token: tokenA);
      expect(a.options.uri.toString(), contains('/app/$appKey'));
      expect(a.options.uri.queryParameters['protocol'], '7');
      await a.connect();
      expect(a.socketId, matches(RegExp(r'^\d+\.\d+$')));
      final established = a.wire.firstWhere((r) => r.direction == 'in');
      expect(established.text, contains('pusher:connection_established'));
    });

    test('a public channel receives a broadcast', () async {
      final news = a.public('announcements');
      expect(await a.subscribe(news), 'ok');
      final got = news.bind(r'App\Events\AnnouncementPosted').first.timeout(const Duration(seconds: 5));
      expect(await api.postJson('/api/announce', tokenA, {'message': 'hello from dart'}), 200);
      final event = await got;
      expect(event.channelName, 'announcements');
      expect(event.tryGetDataAsMap(), {'message': 'hello from dart'});
    });

    test('X-Socket-ID leaves the sender out of its own broadcast', () async {
      final news = a.public('announcements');
      final got = firstOrNull(news.bind(r'App\Events\AnnouncementPosted'), timeout: const Duration(seconds: 2));
      expect(
        await api.postJson('/api/announce', tokenA, {'message': 'not for me'}, socketId: a.socketId),
        200,
      );
      expect(await got, isNull);
    });

    test('a private channel subscribes through /api/broadcasting/auth and receives an event', () async {
      final mine = a.private('private-users.$idA');
      expect(await a.subscribe(mine), 'ok');
      final got = mine.bind(r'App\Events\UserNotified').first.timeout(const Duration(seconds: 5));
      expect(await api.postJson('/api/notify-me', tokenA, {'message': 'just you'}), 200);
      final event = await got;
      expect(event.tryGetDataAsMap(), {'user_id': idA, 'message': 'just you'});
    });

    test("another user's private channel and an undeclared public channel are refused", () async {
      final (status, body) = await api.authorize(tokenA, a.socketId!, 'private-users.$idB');
      expect(status, 403);
      expect(jsonDecode(body), {'error': 'Forbidden'});
      final foreign = a.private('private-users.$idB');
      expect(await a.subscribe(foreign), startsWith('error'));
      final undeclared = a.public('secrets');
      expect(await a.subscribe(undeclared), 'error 403');
    });

    test('presence: the member list, then member_added for a second client', () async {
      roomA = a.presence('presence-chat.1');
      rosterA = PresenceRoster(roomA);
      expect(await a.subscribe(roomA), 'ok');
      expect(rosterA.members, {'$idA': {'name': 'Demo User'}});
      expect(roomA.state?.members?.membersCount, 1);
      expect(roomA.state?.members?.getMyId(), '$idA');

      final added = roomA.whenMemberAdded().first.timeout(const Duration(seconds: 5));
      b = AnvilSession(baseUrl: base, appKey: appKey, token: tokenB);
      await b!.connect();
      final roomB = b!.presence('presence-chat.1');
      final rosterB = PresenceRoster(roomB);
      expect(await b!.subscribe(roomB), 'ok');
      // The roster reads the names from pusher:subscription_succeeded (the package's own list has ids only).
      expect(rosterB.members, {
        '$idA': {'name': 'Demo User'},
        '$idB': {'name': 'Second User'},
      });
      await rosterB.dispose();
      final members = roomB.state!.members!;
      expect(members.membersCount, 2);
      expect(members.getAsMap().keys.toSet(), {'$idA', '$idB'});
      // The package keeps only the ids from the member list (its `info` is null); the wire carries the hash.
      final joined = b!.wire.lastWhere((r) => r.text.contains('pusher_internal:subscription_succeeded'));
      final presence = jsonDecode(jsonDecode(joined.text)['data'] as String)['presence'] as Map<String, dynamic>;
      expect(presence['count'], 2);
      expect(presence['hash'], {
        '$idA': {'name': 'Demo User'},
        '$idB': {'name': 'Second User'},
      });

      final e = await added;
      expect(e.tryGetDataAsMap(), {
        'user_id': '$idB',
        'user_info': {'name': 'Second User'},
      });
      expect(roomA.state?.members?.membersCount, 2);
      expect(rosterA.members['$idB'], {'name': 'Second User'});
    });

    test('whispers reach the other member with user_id, never the sender', () async {
      final roomB = b!.presence('presence-chat.1');
      final atA = roomA.bind('client-typing').first.timeout(const Duration(seconds: 5));
      final echo = firstOrNull(roomB.bind('client-typing'), timeout: const Duration(seconds: 2));
      b!.whisper(roomB, 'typing', {'typing': true});
      final got = await atA;
      expect(got.tryGetDataAsMap(), {'typing': true});
      expect(got.userId, '$idB');
      expect(await echo, isNull);
    });

    test('member_removed when the second client leaves', () async {
      final removed = roomA.whenMemberRemoved().first.timeout(const Duration(seconds: 5));
      await b!.close();
      final e = await removed;
      expect(e.tryGetDataAsMap()?['user_id'], '$idB');
      expect(roomA.state?.members?.membersCount, 1);
      expect(rosterA.members.keys, ['$idA']);
    });

    test('revoking the token closes the socket with 4200; re-authorizing then fails', () async {
      final mine = a.private('private-users.$idA');
      final closed = a.closes.first.timeout(const Duration(seconds: 10));
      final refused = mine.onSubscriptionError().first.timeout(const Duration(seconds: 15));
      final signedOut = a.unauthenticated.first.timeout(const Duration(seconds: 15));
      expect(await api.revoke(tokenA), 204);
      final close = await closed;
      expect(close.code, 4200);
      expect(close.reason, isNotEmpty);
      // The client reconnects at once and asks /api/broadcasting/auth again: the ended token gets 401.
      final err = await refused;
      expect(err.data.toString(), contains('Unauthenticated'));
      await signedOut; // the app's cue to go back to its sign-in
      final after = await http.get(base.resolve('/api/user'), headers: api.bearer(tokenA));
      expect(after.statusCode, 401);
      // The new socket is connected (public channels still work without a token).
      expect(a.socketId, matches(RegExp(r'^\d+\.\d+$')));
      await api.revoke(tokenB);
    });
  });
}
