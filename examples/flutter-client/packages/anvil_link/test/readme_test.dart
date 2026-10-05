// The Flutter section of Smeltery's README, as code: the plain `dart_pusher_channels` calls it shows (no
// anvil_link helpers), run against the server app of this example's README.
// Run: dart test test/readme_test.dart  (tagged `live`; skipped when the app does not answer /api/health)
@Tags(['live'])
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:dart_pusher_channels/dart_pusher_channels.dart';
import 'package:http/http.dart' as http;
import 'package:test/test.dart';

final env = Platform.environment;
final api = Uri.parse(env['SMELTERY_URL'] ?? 'http://127.0.0.1:8000');
final appKey = env['ANVIL_APP_KEY'] ?? 'smeltery-example-key';
final password = env['SMELTERY_PASSWORD'] ?? 'password';

Future<String> signIn(String email, String password) async {
  final res = await http.post(
    api.resolve('/api/tokens'),
    headers: {'Content-Type': 'application/json', 'Accept': 'application/json'},
    body: jsonEncode({'email': email, 'password': password, 'device_name': 'Pixel 9'}),
  );
  if (res.statusCode != 201) throw Exception('sign-in failed: ${res.statusCode}');
  return jsonDecode(res.body)['token'] as String;
}

/// One client as the README sets it up.
class Client {
  Client(this.token, this.userId);

  final String token;
  final int userId;
  final names = <String, String>{};
  final authFailures = StreamController<int>.broadcast();

  late final headers = {'Authorization': 'Bearer $token', 'Accept': 'application/json'};

  late final client = PusherChannelsClient.websocket(
    options: PusherChannelsOptions.fromHost(
      scheme: api.scheme == 'https' ? 'wss' : 'ws',
      host: api.host,
      port: api.port,
      key: appKey, // ANVIL_APP_KEY
    ),
    connectionErrorHandler: (exception, trace, refresh) => refresh(),
  );

  void onAuthFailed(dynamic exception, StackTrace trace) {
    if (exception is EndpointAuthorizableChannelTokenAuthorizationException && exception.response.statusCode == 401) {
      // The token was signed out or revoked: back to the sign-in.
      authFailures.add(401);
    }
  }

  late final authUrl = api.resolve('/api/broadcasting/auth');

  late final news = client.publicChannel('announcements');
  late final mine = client.privateChannel(
    'private-users.$userId',
    authorizationDelegate: EndpointAuthorizableChannelTokenAuthorizationDelegate.forPrivateChannel(
      authorizationEndpoint: authUrl,
      headers: headers,
      onAuthFailed: onAuthFailed,
    ),
  );
  late final room = client.presenceChannel(
    'presence-chat.1',
    authorizationDelegate: EndpointAuthorizableChannelTokenAuthorizationDelegate.forPresenceChannel(
      authorizationEndpoint: authUrl,
      headers: headers,
      onAuthFailed: onAuthFailed,
    ),
  );

  Future<void> start() async {
    final channels = <Channel>[news, mine, room];
    room.whenSubscriptionSucceeded().listen((event) {
      final hash = event.tryGetDataAsMap()?['presence']?['hash'] as Map? ?? {};
      names
        ..clear()
        ..addAll({for (final e in hash.entries) '${e.key}': '${e.value['name']}'});
    });
    room.whenMemberAdded().listen((event) {
      final data = event.tryGetDataAsMap()!;
      names['${data['user_id']}'] = '${data['user_info']['name']}';
    });
    room.whenMemberRemoved().listen((event) => names.remove('${event.tryGetDataAsMap()!['user_id']}'));
    client.onConnectionEstablished.listen((_) {
      for (final channel in channels) {
        channel.subscribeIfNotUnsubscribed();
      }
    });
    final ready = room.whenSubscriptionSucceeded().first;
    await client.connect();
    await ready.timeout(const Duration(seconds: 10));
  }

  Future<void> signOut() async {
    await http.delete(api.resolve('/api/tokens/current'), headers: headers);
    await client.disconnect();
    client.dispose();
  }
}

Future<bool> serverUp() async {
  try {
    final r = await http.get(api.resolve('/api/health')).timeout(const Duration(seconds: 3));
    return r.statusCode == 200;
  } catch (_) {
    return false;
  }
}

void main() async {
  final up = await serverUp();

  test(
    'the README flow: sign in, channels, events, presence names, whispers, revocation',
    skip: up ? null : 'no server at $api',
    () async {
      final tokenA = await signIn(env['SMELTERY_EMAIL'] ?? 'demo@example.com', password);
      final tokenB = await signIn(env['SMELTERY_EMAIL2'] ?? 'second@example.com', password);
      Future<int> idOf(String t) async =>
          jsonDecode(
                (await http.get(
                  api.resolve('/api/user'),
                  headers: {'Authorization': 'Bearer $t', 'Accept': 'application/json'},
                )).body,
              )['id']
              as int;
      final a = Client(tokenA, await idOf(tokenA));
      final b = Client(tokenB, await idOf(tokenB));

      await a.start();
      expect(a.names, {'${a.userId}': 'Demo User'});

      // Events on the public and the private channel.
      final announced = a.news.bind(r'App\Events\AnnouncementPosted').first.timeout(const Duration(seconds: 5));
      final notified = a.mine.bind(r'App\Events\UserNotified').first.timeout(const Duration(seconds: 5));
      final post = {...a.headers, 'Content-Type': 'application/json'};
      await http.post(api.resolve('/api/announce'), headers: post, body: jsonEncode({'message': 'hi all'}));
      await http.post(api.resolve('/api/notify-me'), headers: post, body: jsonEncode({'message': 'hi me'}));
      expect((await announced).tryGetDataAsMap(), {'message': 'hi all'});
      expect((await notified).tryGetDataAsMap(), {'user_id': a.userId, 'message': 'hi me'});

      // Presence names: the second client sees both from the subscription; the first gets member_added.
      final added = a.room.whenMemberAdded().first.timeout(const Duration(seconds: 5));
      await b.start();
      expect(b.names, {'${a.userId}': 'Demo User', '${b.userId}': 'Second User'});
      await added;
      expect(a.names, {'${a.userId}': 'Demo User', '${b.userId}': 'Second User'});

      // Whispers.
      final typing = a.room.bind('client-typing').first.timeout(const Duration(seconds: 5));
      b.room.trigger(eventName: 'client-typing', data: {'typing': true});
      final whisper = await typing;
      expect(whisper.userId, '${b.userId}');
      expect(whisper.tryGetDataAsMap(), {'typing': true});

      // Sign-out of the second client: the first sees member_removed.
      final removed = a.room.whenMemberRemoved().first.timeout(const Duration(seconds: 5));
      await b.signOut();
      await removed;
      expect(a.names, {'${a.userId}': 'Demo User'});

      // Revocation of the first client's token elsewhere: close 4200, reconnect, authorization 401 → onAuthFailed.
      final failed = a.authFailures.stream.first.timeout(const Duration(seconds: 15));
      final revoked = await http.delete(api.resolve('/api/tokens/current'), headers: a.headers);
      expect(revoked.statusCode, 204);
      expect(await failed, 401);
      await a.client.disconnect();
      a.client.dispose();
    },
  );
}
