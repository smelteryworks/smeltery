import 'dart:async';

import 'package:dart_pusher_channels/dart_pusher_channels.dart';

import 'wire.dart';

/// One client's socket to a Smeltery app's Anvil endpoint, authorizing private and presence channels with a
/// Hallmark bearer token at `POST /api/broadcasting/auth`.
class AnvilSession {
  AnvilSession({
    required this.baseUrl,
    required this.appKey,
    required this.token,
  }) {
    final secure = baseUrl.scheme == 'https';
    // The package builds `ws(s)://host:port/app/<key>?client=dart&version=…&protocol=7`.
    options = PusherChannelsOptions.fromHost(
      scheme: secure ? 'wss' : 'ws',
      host: baseUrl.host,
      port: baseUrl.hasPort ? baseUrl.port : null,
      key: appKey,
    );
    client = PusherChannelsClient.custom(
      connectionDelegate: () => RecordingConnection(options.uri, wire, _closes),
      connectionErrorHandler: (exception, trace, refresh) {
        wire.add(WireRecord('close', 'connection error $exception'));
        refresh();
      },
      minimumReconnectDelayDuration: const Duration(milliseconds: 500),
    );
    _established = client.onConnectionEstablished.listen((_) {
      // After a reconnect (a 4200 close, a network drop) the channels authorize and subscribe again.
      for (final c in _channels) {
        c.subscribeIfNotUnsubscribed();
      }
    });
  }

  final Uri baseUrl;
  final String appKey;
  String token;

  late final PusherChannelsOptions options;
  late final PusherChannelsClient client;

  /// Every frame in and out, and every close.
  final List<WireRecord> wire = [];
  final StreamController<SocketClose> _closes = StreamController.broadcast();
  final List<Channel> _channels = [];
  late final StreamSubscription<void> _established;

  final StreamController<void> _unauthenticated = StreamController.broadcast();

  /// The closes of the socket with their codes.
  Stream<SocketClose> get closes => _closes.stream;

  /// Fires when `/api/broadcasting/auth` answers 401: the token was signed out or revoked (after a 4200 close the
  /// client reconnects and authorizes again, which fails then). The app goes back to its sign-in.
  Stream<void> get unauthenticated => _unauthenticated.stream;

  String? get socketId => client.socketId;

  Uri get authEndpoint => baseUrl.resolve('/api/broadcasting/auth');

  /// The header the authorizer sends; read at each authorization, so a new token applies on the next one.
  Map<String, String> get _authHeaders => {'Authorization': 'Bearer $token', 'Accept': 'application/json'};

  /// Connects and waits for `pusher:connection_established`.
  Future<void> connect({Duration timeout = const Duration(seconds: 10)}) async {
    final up = client.onConnectionEstablished.first;
    unawaited(client.connect());
    await up.timeout(timeout);
  }

  PublicChannel public(String name) => _track(client.publicChannel(name));

  PrivateChannel private(String name) => _track(
        client.privateChannel(
          name,
          authorizationDelegate: _BearerDelegate<PrivateChannelAuthorizationData>(
            this,
            EndpointAuthorizableChannelTokenAuthorizationDelegate.forPrivateChannel,
          ),
        ),
      );

  PresenceChannel presence(String name) => _track(
        client.presenceChannel(
          name,
          authorizationDelegate: _BearerDelegate<PresenceChannelAuthorizationData>(
            this,
            EndpointAuthorizableChannelTokenAuthorizationDelegate.forPresenceChannel,
          ),
        ),
      );

  T _track<T extends Channel>(T channel) {
    if (!_channels.contains(channel)) _channels.add(channel);
    return channel;
  }

  /// Subscribes and waits for the outcome: `ok`, `error <type/status>` or `timeout`.
  Future<String> subscribe(Channel channel, {Duration timeout = const Duration(seconds: 6)}) async {
    final outcome = Completer<String>();
    final ok = channel.whenSubscriptionSucceeded().listen((_) {
      if (!outcome.isCompleted) outcome.complete('ok');
    });
    final err = channel.onSubscriptionError().listen((e) {
      final data = e.tryGetDataAsMap();
      final what = data?['status'] ?? data?['type'] ?? e.data;
      if (!outcome.isCompleted) outcome.complete('error $what');
    });
    channel.subscribe();
    try {
      return await outcome.future.timeout(timeout, onTimeout: () => 'timeout');
    } finally {
      await ok.cancel();
      await err.cancel();
    }
  }

  /// Sends a client event (`client-<name>`) on a private or presence channel.
  void whisper(Channel channel, String name, Map<String, dynamic> data) {
    switch (channel) {
      case PrivateChannel c:
        c.trigger(eventName: 'client-$name', data: data);
      case PresenceChannel c:
        c.trigger(eventName: 'client-$name', data: data);
      default:
        throw ArgumentError('client events need a private or presence channel');
    }
  }

  Future<void> close() async {
    await _established.cancel();
    if (!client.isDisposed) {
      await client.disconnect();
      client.dispose();
    }
    await _closes.close();
    await _unauthenticated.close();
  }
}

/// The package's token delegate, built at each authorization with the session's current bearer header.
class _BearerDelegate<T extends EndpointAuthorizationData>
    implements EndpointAuthorizableChannelAuthorizationDelegate<T> {
  _BearerDelegate(this.session, this.factory);

  final AnvilSession session;
  final EndpointAuthorizableChannelTokenAuthorizationDelegate<T> Function({
    required Uri authorizationEndpoint,
    required Map<String, String> headers,
    bool overrideContentTypeHeader,
    EndpointAuthorizableChannelTokenAuthorizationParser<T> parser,
    EndpointAuthFailedCallback? onAuthFailed,
  }) factory;

  @override
  EndpointAuthFailedCallback? get onAuthFailed => null;

  @override
  Future<T> authorizationData(String socketId, String channelName) async {
    try {
      return await factory(
        authorizationEndpoint: session.authEndpoint,
        headers: session._authHeaders,
      ).authorizationData(socketId, channelName);
    } on EndpointAuthorizableChannelTokenAuthorizationException catch (e) {
      if (e.response.statusCode == 401 && !session._unauthenticated.isClosed) {
        session._unauthenticated.add(null);
      }
      rethrow;
    }
  }
}
