//! Random tokens, hashing and constant-time comparison (the one home of these in Smeltery), and the keys derived
//! from `APP_KEY` behind [`App::sign`], [`App::encrypt`] and the session cookies.
//!
//! ```
//! use smeltery_core::crypto::{constant_time_eq, random_token, sha256_hex};
//!
//! let token = random_token(40)?;
//! assert_eq!(token.len(), 40);
//! let stored = sha256_hex(&token);
//! assert!(constant_time_eq(&stored, &sha256_hex(&token)));
//! # Ok::<(), smeltery_core::Error>(())
//! ```

use std::sync::Arc;

use aes_gcm::aead::{Aead, Payload};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::app::App;
use crate::config::Settings;
use crate::error::{Error, Result};

/// The keys derived from `APP_KEY`. Never printed.
#[derive(Clone)]
pub(crate) struct Keys {
    pub(crate) cookie: cookie::Key,
    /// The master key bytes, for purpose-bound signing keys ([`App::sign`]).
    master: Arc<[u8]>,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keys(<redacted>)")
    }
}

/// A fixed key for `APP_ENV=testing` without an `APP_KEY` (tests need no secret). It is public, so it is used
/// only when `APP_KEY` is empty (never for a malformed or short one), and `serve` / `work` refuse to run with it.
const TEST_KEY: &[u8; 32] = b"smeltery-testing-key-not-secret!";

impl Keys {
    /// The keys for these settings.
    ///
    /// # Errors
    /// `APP_KEY` is malformed or shorter than 32 bytes, or missing outside `APP_ENV=testing`.
    pub(crate) fn from_settings(settings: &Settings) -> Result<Self> {
        let master = match crate::config::app_key_bytes(&settings.key) {
            Some(master) => master,
            None if settings.uses_test_key() => TEST_KEY.to_vec(),
            None => {
                return Err(Error::internal(
                    "APP_KEY is missing, malformed or shorter than 32 bytes: run `smeltery key:generate` \
                     (sessions and cookies are encrypted with it)",
                ));
            }
        };
        if settings.is_production() && !settings.key.starts_with("base64:") {
            // Plain text of 32 characters (a passphrase) holds far less than 256 bits of chance.
            tracing::warn!(
                "APP_KEY is plain text, not a `base64:` key from `key:generate`: a typed passphrase is far \
                 easier to guess than 32 random bytes; replace it with the output of `key:generate --show`"
            );
        }
        // HKDF-SHA256 from the master key into the cookie crate's signing + encryption keys.
        Ok(Self {
            cookie: cookie::Key::derive_from(&master),
            master: Arc::from(master),
        })
    }

    /// The framework's own encryption key for `purpose`: HMAC-SHA256(master, `"smeltery-key:" ‖ purpose`).
    fn key(&self, purpose: &str) -> Result<[u8; 32]> {
        self.labelled(APP_KEY_LABEL_INTERNAL, purpose)
    }

    /// A key of [`App::derive_key`]: HMAC-SHA256(master, `"smeltery-app-key:" ‖ purpose`).
    fn app_key(&self, purpose: &str) -> Result<[u8; 32]> {
        self.labelled(APP_KEY_LABEL_PUBLIC, purpose)
    }

    /// The AES key of [`App::encrypt`]: HMAC-SHA256(master, `"smeltery-app-enc:" ‖ purpose`), never a
    /// [`App::derive_key`] key, so a derived key handed out for a purpose never opens values encrypted for it.
    fn encryption_key(&self, purpose: &str) -> Result<[u8; 32]> {
        self.labelled(APP_KEY_LABEL_ENCRYPT, purpose)
    }

    fn labelled(&self, label: &[u8], purpose: &str) -> Result<[u8; 32]> {
        let mut derive = Hmac::<Sha256>::new_from_slice(&self.master)
            .map_err(|_| Error::internal("invalid HMAC key length"))?;
        derive.update(label);
        derive.update(purpose.as_bytes());
        Ok(derive.finalize().into_bytes().into())
    }

    /// HMAC-SHA256 of `message` under the key for `purpose`: HMAC-SHA256(master, "smeltery:" + purpose).
    fn mac(&self, purpose: &str, message: &[u8]) -> Result<[u8; 32]> {
        let invalid = |_| Error::internal("invalid HMAC key length");
        let mut derive = Hmac::<Sha256>::new_from_slice(&self.master).map_err(invalid)?;
        derive.update(b"smeltery:");
        derive.update(purpose.as_bytes());
        let key = derive.finalize().into_bytes();
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).map_err(invalid)?;
        mac.update(message);
        Ok(mac.finalize().into_bytes().into())
    }
}

/// The label of the framework's internal keys (PubSub).
const APP_KEY_LABEL_INTERNAL: &[u8] = b"smeltery-key:";
/// The label of the public API's keys; differs from `smeltery-key:` and the signing label `smeltery:` at a fixed
/// position, so no internal key, public key or signing key is ever another.
const APP_KEY_LABEL_PUBLIC: &[u8] = b"smeltery-app-key:";
/// The label of [`App::encrypt`]'s keys (differs from `smeltery-app-key:` at a fixed position).
const APP_KEY_LABEL_ENCRYPT: &[u8] = b"smeltery-app-enc:";

impl App {
    /// Sign `message` for `purpose`: base64url (no padding) of HMAC-SHA256 under a key derived from `APP_KEY`
    /// for that purpose, so a signature made for one purpose never verifies for another.
    ///
    /// ```
    /// # async fn demo() -> smeltery_core::Result<()> {
    /// use smeltery_core::{AppBuilder, config::Settings};
    ///
    /// let mut settings = Settings::from_env();
    /// settings.key = "0123456789abcdef0123456789abcdef".into();
    /// let app = AppBuilder::new(settings).build().await?.app;
    /// let signature = app.sign("invites", b"user=7")?;
    /// assert!(app.verify_signature("invites", b"user=7", &signature));
    /// assert!(!app.verify_signature("invites", b"user=8", &signature));
    /// assert!(!app.verify_signature("other", b"user=7", &signature));
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// The app has no usable `APP_KEY`.
    pub fn sign(&self, purpose: &str, message: &[u8]) -> Result<String> {
        let keys = self.signing_keys()?;
        let mac = keys.mac(purpose, message)?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac))
    }

    /// Whether `signature` is [`App::sign`]'s signature of `message` for `purpose` (compared in constant time).
    pub fn verify_signature(&self, purpose: &str, message: &[u8], signature: &str) -> bool {
        self.sign(purpose, message)
            .is_ok_and(|expected| same(&expected, signature))
    }

    /// A 32-byte encryption key for `purpose`: HMAC-SHA256(master, `"smeltery-key:" ‖ purpose`). Its own label
    /// (`smeltery-key:`, never `smeltery:`) keeps keys apart from signatures: a signature is an HMAC under a key
    /// derived with `"smeltery:" ‖ purpose`, so no [`App::sign`] output, for any purpose and message, is a key
    /// (D-403 amendment). The framework's own encryption (PubSub); the public API uses [`App::derive_key`].
    pub(crate) fn purpose_key(&self, purpose: &str) -> Result<[u8; 32]> {
        self.signing_keys()?.key(purpose)
    }

    /// A 32-byte key for `purpose`, derived from `APP_KEY`: HMAC-SHA256(`APP_KEY`, `"smeltery-app-key:" ‖ purpose`).
    /// Its label keeps it apart from [`App::sign`]'s signing keys and from the framework's internal keys (PubSub), so
    /// no signature shown to a client and no internal key is ever such a key. Every purpose gives an unrelated key;
    /// framework crates use dotted purposes (`anvil.secret`, `temper.two-factor`), apps plain ones. Rotating
    /// `APP_KEY` changes every key.
    ///
    /// # Errors
    /// The app has no usable `APP_KEY`.
    pub fn derive_key(&self, purpose: &str) -> Result<[u8; 32]> {
        self.signing_keys()?.app_key(purpose)
    }

    /// Encrypt and authenticate `plaintext` for `purpose`: AES-256-GCM under a key derived from `APP_KEY` for the
    /// purpose (HMAC-SHA256(`APP_KEY`, `"smeltery-app-enc:" ‖ purpose`): never a signing key, a framework key or an
    /// [`App::derive_key`] key, so handing out `derive_key(p)` never opens values encrypted under `p`) with a random
    /// 96-bit nonce, as `base64(nonce ‖ ciphertext ‖ tag)` (standard alphabet). `aad` (associated
    /// data, e.g. the id of the user a secret belongs to) is authenticated but not stored: opening needs the same
    /// `aad`, so a value copied to another user's row does not open there.
    ///
    /// ```
    /// # async fn demo() -> smeltery_core::Result<()> {
    /// use smeltery_core::{AppBuilder, config::Settings};
    ///
    /// let mut settings = Settings::from_env();
    /// settings.key = "0123456789abcdef0123456789abcdef".into();
    /// let app = AppBuilder::new(settings).build().await?.app;
    /// let sealed = app.encrypt("notes", b"user:7", b"meet at noon")?;
    /// assert_eq!(app.decrypt("notes", b"user:7", &sealed)?.as_deref(), Some(&b"meet at noon"[..]));
    /// assert_eq!(app.decrypt("notes", b"user:8", &sealed)?, None);
    /// assert_eq!(app.decrypt("other", b"user:7", &sealed)?, None);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// The app has no usable `APP_KEY`, or the OS random source fails.
    pub fn encrypt(&self, purpose: &str, aad: &[u8], plaintext: &[u8]) -> Result<String> {
        Cipher::new(&self.signing_keys()?.encryption_key(purpose)?)?.seal(aad, plaintext)
    }

    /// Open a value from [`App::encrypt`] with the same `purpose` and `aad`: `Ok(None)` when it does not
    /// authenticate (another purpose, `aad` or `APP_KEY`; an edited or truncated value; not base64).
    ///
    /// # Errors
    /// The app has no usable `APP_KEY`.
    pub fn decrypt(&self, purpose: &str, aad: &[u8], ciphertext: &str) -> Result<Option<Vec<u8>>> {
        Ok(Cipher::new(&self.signing_keys()?.encryption_key(purpose)?)?.open(aad, ciphertext))
    }

    fn signing_keys(&self) -> Result<&Keys> {
        self.web_config().map(|w| &w.keys).ok_or_else(|| {
            Error::internal(
                "APP_KEY is missing or shorter than 32 bytes: run `smeltery key:generate`",
            )
        })
    }
}

/// The AES-GCM nonce length.
const NONCE_LEN: usize = 12;

/// AES-256-GCM under one key: `base64(nonce ‖ ciphertext ‖ tag)` with a random 96-bit nonce. The only AES-GCM code
/// in Smeltery: [`App::encrypt`] and PubSub (with an empty `aad`) use it.
pub(crate) struct Cipher(aes_gcm::Aes256Gcm);

impl Cipher {
    pub(crate) fn new(key: &[u8; 32]) -> Result<Self> {
        <aes_gcm::Aes256Gcm as aes_gcm::KeyInit>::new_from_slice(key)
            .map(Self)
            .map_err(|_| Error::internal("invalid encryption key length"))
    }

    pub(crate) fn seal(&self, aad: &[u8], plain: &[u8]) -> Result<String> {
        let nonce = random_bytes(NONCE_LEN)?;
        let sealed = self
            .0
            .encrypt(&nonce_of(&nonce)?, Payload { msg: plain, aad })
            .map_err(|_| Error::internal("encryption failed"))?;
        let mut out = nonce;
        out.extend_from_slice(&sealed);
        Ok(base64::engine::general_purpose::STANDARD.encode(out))
    }

    pub(crate) fn open(&self, aad: &[u8], text: &str) -> Option<Vec<u8>> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text.trim())
            .ok()?;
        if bytes.len() <= NONCE_LEN {
            return None;
        }
        let (nonce, sealed) = bytes.split_at(NONCE_LEN);
        self.0
            .decrypt(&nonce_of(nonce).ok()?, Payload { msg: sealed, aad })
            .ok()
    }
}

/// A 12-byte AES-GCM nonce from a slice of that length.
fn nonce_of(bytes: &[u8]) -> Result<aes_gcm::Nonce<aes_gcm::aead::consts::U12>> {
    let array: [u8; NONCE_LEN] = bytes
        .try_into()
        .map_err(|_| Error::internal("invalid nonce length"))?;
    Ok(array.into())
}

/// `len` random characters from `[A-Za-z0-9]`, from the OS random source, every character equally likely
/// (rejection sampling): 40 characters hold about 238 bits.
///
/// # Errors
/// The OS random source fails.
pub fn random_token(len: usize) -> Result<String> {
    const ALPHABET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(len);
    let mut buf = [0u8; 64];
    while out.len() < len {
        getrandom::fill(&mut buf).map_err(|e| Error::internal(format!("no random source: {e}")))?;
        for b in buf {
            // 248 = 4 * 62: rejecting the rest keeps every character equally likely.
            if b < 248
                && let Some(c) = ALPHABET.get(usize::from(b % 62))
            {
                out.push(char::from(*c));
                if out.len() == len {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// `len` random bytes from the OS random source.
///
/// # Errors
/// The OS random source fails.
pub fn random_bytes(len: usize) -> Result<Vec<u8>> {
    let mut out = vec![0u8; len];
    getrandom::fill(&mut out).map_err(|e| Error::internal(format!("no random source: {e}")))?;
    Ok(out)
}

/// SHA-256 of `text`, as lowercase hex (64 characters).
pub fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether two secrets are equal, compared in constant time for equal lengths (a length difference answers `false`
/// at once, so compare values of a fixed length, such as hashes). Every comparison of a secret (a token, a code, a
/// signature, a hash of either) in Smeltery goes through it.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// [`constant_time_eq`], under the short name the framework's own code uses.
pub(crate) fn same(a: &str, b: &str) -> bool {
    constant_time_eq(a, b)
}

/// The CSRF token as a page shows it: `base64url(pad || pad XOR token)` with a fresh random
/// pad each time, so the bytes differ in every response while [`csrf_matches`] accepts them
/// all. A compressed page then leaks nothing about the token through its length (BREACH).
pub(crate) fn mask_csrf(token: &str) -> Result<String> {
    let pad = random_bytes(token.len())?;
    let mut out = pad.clone();
    out.extend(pad.iter().zip(token.as_bytes()).map(|(p, t)| p ^ t));
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out))
}

/// Whether `submitted` (a masked token from a page, or the session token itself) is the
/// session's CSRF `token`; constant time for the comparison.
pub(crate) fn csrf_matches(submitted: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(submitted)
        && bytes.len() == token.len() * 2
    {
        let (pad, masked) = bytes.split_at(token.len());
        let unmasked: Vec<u8> = pad.iter().zip(masked).map(|(p, m)| p ^ m).collect();
        return unmasked.ct_eq(token.as_bytes()).into();
    }
    same(submitted, token)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review M1: a purpose key was `sign(purpose, "smeltery:key")`, so a public signature could be a key.
    #[tokio::test]
    async fn encryption_keys_are_never_signatures() {
        let mut s = Settings::from_env();
        s.key = "crypto-test-key-0123456789abcdef!!".into();
        let app = crate::AppBuilder::new(s).build().await.unwrap().app;
        let key = app.purpose_key("pubsub").unwrap();
        for message in [&b"smeltery:key"[..], b"", b"pubsub", b"smeltery-key:pubsub"] {
            for purpose in ["pubsub", "", "key", "smeltery-key:pubsub"] {
                let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(app.sign(purpose, message).unwrap())
                    .unwrap();
                assert_ne!(signature, key, "{purpose} / {message:?}");
            }
        }
        // The derivation is fixed: HMAC-SHA256(master, "smeltery-key:" ‖ purpose).
        let mut mac =
            Hmac::<Sha256>::new_from_slice(b"crypto-test-key-0123456789abcdef!!").unwrap();
        mac.update(b"smeltery-key:pubsub");
        assert_eq!(key, <[u8; 32]>::from(mac.finalize().into_bytes()));
        assert_ne!(key, app.purpose_key("other").unwrap());
    }

    /// Review M1: `encrypt("pubsub", b"", …)` sealed under PubSub's own key, so app code could forge or read bus
    /// messages. The public API's keys now have their own label.
    #[tokio::test]
    async fn public_keys_never_open_the_frameworks_messages() {
        let mut s = Settings::from_env();
        s.key = "crypto-test-key-0123456789abcdef!!".into();
        let app = crate::AppBuilder::new(s).build().await.unwrap().app;
        let internal = Cipher::new(&app.purpose_key("pubsub").unwrap()).unwrap();
        let bus = internal.seal(b"", b"{\"v\":1}").unwrap();
        assert_eq!(app.decrypt("pubsub", b"", &bus).unwrap(), None);
        let forged = app.encrypt("pubsub", b"", b"{\"v\":1}").unwrap();
        assert_eq!(internal.open(b"", &forged), None);
        assert_ne!(
            app.derive_key("pubsub").unwrap(),
            app.purpose_key("pubsub").unwrap()
        );
        // Review RA-N2: a handed-out `derive_key(p)` is not the key of `encrypt(p, …)`, both ways.
        let derived = Cipher::new(&app.derive_key("anvil.secret").unwrap()).unwrap();
        let sealed = app.encrypt("anvil.secret", b"", b"private").unwrap();
        assert_eq!(derived.open(b"", &sealed), None);
        let by_derived = derived.seal(b"", b"forged").unwrap();
        assert_eq!(app.decrypt("anvil.secret", b"", &by_derived).unwrap(), None);
        // The derivation is fixed: HMAC-SHA256(master, "smeltery-app-key:" ‖ purpose), and never a signature.
        let mut mac =
            Hmac::<Sha256>::new_from_slice(b"crypto-test-key-0123456789abcdef!!").unwrap();
        mac.update(b"smeltery-app-key:anvil.secret");
        let key = app.derive_key("anvil.secret").unwrap();
        assert_eq!(key, <[u8; 32]>::from(mac.finalize().into_bytes()));
        for purpose in ["anvil.secret", "", "app-key:anvil.secret"] {
            let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(app.sign(purpose, b"").unwrap())
                .unwrap();
            assert_ne!(signature, key);
        }
    }

    #[test]
    fn keys_need_32_bytes_outside_testing() {
        let mut s = Settings::from_env();
        s.env = "local".into();
        s.key = "short".into();
        let err = Keys::from_settings(&s).unwrap_err().to_string();
        assert!(err.contains("smeltery key:generate"), "{err}");
        s.key = format!(
            "base64:{}",
            base64::engine::general_purpose::STANDARD.encode([7u8; 32])
        );
        assert!(Keys::from_settings(&s).is_ok());
        s.key = "x".repeat(32);
        assert!(Keys::from_settings(&s).is_ok());
        s.key = String::new();
        s.env = "testing".into();
        assert!(Keys::from_settings(&s).is_ok());
        assert_eq!(
            format!("{:?}", Keys::from_settings(&s).unwrap()),
            "Keys(<redacted>)"
        );
    }

    #[test]
    fn a_plain_text_key_in_production_is_warned_about() {
        let mut s = Settings::from_env();
        s.env = "production".into();
        s.key = "correct horse battery staple 123".into();
        let logged = crate::logging::capture(|| {
            assert!(Keys::from_settings(&s).is_ok());
        });
        assert!(logged.contains("APP_KEY is plain text"), "{logged}");
        s.key = format!(
            "base64:{}",
            base64::engine::general_purpose::STANDARD.encode([7u8; 32])
        );
        let logged = crate::logging::capture(|| {
            assert!(Keys::from_settings(&s).is_ok());
        });
        assert!(!logged.contains("APP_KEY"), "{logged}");
    }

    #[test]
    fn testing_never_replaces_a_bad_key_with_the_public_one() {
        let mut s = Settings::from_env();
        s.env = "testing".into();
        // A typo in the base64 text, a short key, a short base64 key: errors, not the public test key.
        for key in [
            "base64:not*base64",
            "short",
            "base64:c2hvcnQ=",
            &"x".repeat(31),
        ] {
            s.key = key.to_owned();
            let err = Keys::from_settings(&s).unwrap_err().to_string();
            assert!(err.contains("key:generate"), "{key}: {err}");
            assert!(!s.uses_test_key());
        }
        s.key = String::new();
        assert!(s.uses_test_key());
        assert!(Keys::from_settings(&s).is_ok());
        s.env = "local".into();
        assert!(!s.uses_test_key());
        assert!(Keys::from_settings(&s).is_err());
    }

    #[test]
    fn tokens_hashes_and_comparison() {
        let a = random_token(40).unwrap();
        assert_eq!(a.len(), 40);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(a, random_token(40).unwrap());
        assert_eq!(random_bytes(16).unwrap().len(), 16);
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "ab"));
    }
}
