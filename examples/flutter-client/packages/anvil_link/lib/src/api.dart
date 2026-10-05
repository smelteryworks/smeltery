import 'dart:convert';

import 'package:http/http.dart' as http;

/// An HTTP failure from the Smeltery app, with the status and body.
class ApiException implements Exception {
  ApiException(this.what, this.status, this.body);

  final String what;
  final int status;
  final String body;

  @override
  String toString() => 'ApiException($what: $status $body)';
}

/// The Smeltery app's HTTP API as a mobile client uses it: tokens and a few JSON routes.
class SmelteryApi {
  SmelteryApi(this.baseUrl, {http.Client? client}) : _http = client ?? http.Client();

  /// The app's origin, such as `http://127.0.0.1:8000`.
  final Uri baseUrl;
  final http.Client _http;

  Uri url(String path) => baseUrl.resolve(path);

  /// `POST /api/tokens`: a Hallmark token for an e-mail address and a password (201 `{"token": "smt_…", …}`).
  Future<String> issueToken({
    required String email,
    required String password,
    required String deviceName,
  }) async {
    final res = await _http.post(
      url('/api/tokens'),
      headers: {'Content-Type': 'application/json', 'Accept': 'application/json'},
      body: jsonEncode({'email': email, 'password': password, 'device_name': deviceName}),
    );
    if (res.statusCode != 201) {
      throw ApiException('POST /api/tokens', res.statusCode, res.body);
    }
    final token = (jsonDecode(res.body) as Map<String, dynamic>)['token'];
    if (token is! String) {
      throw ApiException('POST /api/tokens (no token)', res.statusCode, res.body);
    }
    return token;
  }

  Map<String, String> bearer(String token) => {
        'Authorization': 'Bearer $token',
        'Accept': 'application/json',
      };

  /// `GET /api/user`: the token's user as JSON.
  Future<Map<String, dynamic>> user(String token) async {
    final res = await _http.get(url('/api/user'), headers: bearer(token));
    if (res.statusCode != 200) {
      throw ApiException('GET /api/user', res.statusCode, res.body);
    }
    return jsonDecode(res.body) as Map<String, dynamic>;
  }

  /// `DELETE /api/tokens/current`: signs the token out (204).
  Future<int> revoke(String token) async {
    final res = await _http.delete(url('/api/tokens/current'), headers: bearer(token));
    return res.statusCode;
  }

  /// POSTs JSON to an app route with the bearer token and returns the status.
  Future<int> postJson(String path, String token, Map<String, dynamic> body, {String? socketId}) async {
    final res = await _http.post(
      url(path),
      headers: {
        ...bearer(token),
        'Content-Type': 'application/json',
        'X-Socket-ID': ?socketId,
      },
      body: jsonEncode(body),
    );
    return res.statusCode;
  }

  /// POSTs the channel authorization form as the Pusher clients send it, returning status and body (for checks).
  Future<(int, String)> authorize(String token, String socketId, String channel) async {
    final res = await _http.post(
      url('/api/broadcasting/auth'),
      headers: {...bearer(token), 'Content-Type': 'application/x-www-form-urlencoded'},
      body: {'socket_id': socketId, 'channel_name': channel},
    );
    return (res.statusCode, res.body);
  }

  void close() => _http.close();
}
