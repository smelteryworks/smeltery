/// A small layer over `dart_pusher_channels` for a Smeltery app's Anvil socket (Pusher protocol 7):
/// Hallmark bearer tokens (`POST /api/tokens`, `DELETE /api/tokens/current`), the socket with a custom host,
/// port and scheme, and channel authorization at `POST /api/broadcasting/auth` with `Authorization: Bearer`.
library;

export 'package:dart_pusher_channels/dart_pusher_channels.dart';

export 'src/api.dart';
export 'src/roster.dart';
export 'src/session.dart';
export 'src/wire.dart';
