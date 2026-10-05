import 'dart:async';

import 'package:dart_pusher_channels/dart_pusher_channels.dart';
import 'package:web_socket_channel/web_socket_channel.dart';

/// One frame or close seen on the socket, for checks and wire evidence.
class WireRecord {
  WireRecord(this.direction, this.text) : at = DateTime.now();

  /// `in`, `out` or `close`.
  final String direction;
  final String text;
  final DateTime at;

  @override
  String toString() => '${at.toIso8601String()} $direction $text';
}

/// A socket close with its WebSocket close code (4200 when the server ends a revoked credential's subscriptions).
class SocketClose {
  SocketClose(this.code, this.reason);

  final int? code;
  final String? reason;

  @override
  String toString() => 'close $code ${reason ?? ''}';
}

/// A `PusherChannelsConnection` like the package's own WebSocket connection, which also records every frame and
/// the close code. The package reconnects on any close and does not expose the code, so this connection does.
class RecordingConnection implements PusherChannelsConnection {
  RecordingConnection(this.uri, this.log, this.closes);

  final Uri uri;
  final List<WireRecord> log;
  final StreamController<SocketClose> closes;

  WebSocketChannel? _channel;
  StreamSubscription<dynamic>? _sub;
  bool _closed = false;

  @override
  void connect({
    required PusherChannelsConnectionOnDoneCallback onDoneCallback,
    required PusherChannelsConnectionOnErrorCallback onErrorCallback,
    required PusherChannelsConnectionOnEventCallback onEventCallback,
  }) {
    final channel = _channel ??= WebSocketChannel.connect(uri);
    _sub ??= channel.stream.listen(
      (event) {
        if (_closed) return;
        final text = event.toString();
        log.add(WireRecord('in', text));
        onEventCallback(text);
      },
      cancelOnError: true,
      onDone: () {
        final close = SocketClose(channel.closeCode, channel.closeReason);
        log.add(WireRecord('close', close.toString()));
        if (!closes.isClosed) closes.add(close);
        if (_closed) return;
        onDoneCallback();
      },
      onError: (Object e, StackTrace t) {
        log.add(WireRecord('close', 'error $e'));
        if (_closed) return;
        _channel = null;
        onErrorCallback(e, t);
      },
    );
  }

  @override
  void sendEvent(String eventEncoded) {
    if (_closed) return;
    log.add(WireRecord('out', eventEncoded));
    _channel?.sink.add(eventEncoded);
  }

  @override
  void ping() => sendEvent('{"event":"pusher:ping","data":{}}');

  @override
  Future<void> close() async {
    if (_closed) return;
    _closed = true;
    await _sub?.cancel();
    await _channel?.sink.close();
  }
}
