//! SMTP over TLS against an in-process server: implicit TLS (`MAIL_ENCRYPTION=tls`), STARTTLS, and a server
//! certificate the mailer does not trust. The certificates are made at test time by a throwaway CA, which the
//! mailer trusts through `MAIL_TLS_CA`. No network: everything runs on 127.0.0.1.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use smeltery_core::config::Settings;
use smeltery_mail::{Address, Email, Envelope, MailSettings, SmtpTransport, Transport};
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader};
use tokio_rustls::TlsAcceptor;

const PASSWORD: &str = "tls-Secret-pass-987";

/// A CA made for one test, and a server certificate for `localhost` / `127.0.0.1` signed by it.
struct Pki {
    _dir: tempfile::TempDir,
    ca_pem: PathBuf,
    acceptor: TlsAcceptor,
}

fn pki() -> Pki {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.distinguished_name
        .push(DnType::CommonName, "Smeltery test CA");
    let ca_cert = ca.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca, ca_key);

    let server_key = KeyPair::generate().unwrap();
    let mut server =
        CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()]).unwrap();
    server.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_cert = server.signed_by(&server_key, &issuer).unwrap();

    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![
            server_cert.der().clone(),
            CertificateDer::from(ca_cert.der().to_vec()),
        ],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der())),
    )
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let ca_pem = dir.path().join("ca.pem");
    std::fs::write(&ca_pem, ca_cert.pem()).unwrap();
    Pki {
        _dir: dir,
        ca_pem,
        acceptor: TlsAcceptor::from(Arc::new(config)),
    }
}

/// What the server saw: each command with whether it came over TLS, the logins and the messages.
#[derive(Clone, Default)]
struct Seen {
    commands: Arc<Mutex<Vec<(String, bool)>>>,
    auth: Arc<Mutex<Vec<String>>>,
    messages: Arc<Mutex<Vec<String>>>,
    handshake_errors: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// TLS from the first byte (port 465 style).
    Implicit,
    /// Plain greeting, `STARTTLS` advertised, then TLS.
    StartTls,
}

/// One SMTP session over `stream` until `QUIT`. With `starttls` set, `STARTTLS` ends the plain session and
/// hands back the stream for the handshake.
async fn session<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    greet: bool,
    tls: bool,
    starttls: bool,
    seen: &Seen,
) -> Option<S> {
    let mut reader = BufReader::new(stream);
    if greet {
        reader
            .get_mut()
            .write_all(b"220 fake ESMTP\r\n")
            .await
            .ok()?;
    }
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let text = line.trim_end().to_owned();
        let upper = text.to_ascii_uppercase();
        let verb = upper.split(' ').next().unwrap_or_default().to_owned();
        seen.commands.lock().unwrap().push((verb.clone(), tls));
        let reply: &[u8] = match verb.as_str() {
            "EHLO" | "HELO" if starttls => b"250-fake\r\n250-STARTTLS\r\n250 8BITMIME\r\n",
            "EHLO" | "HELO" => b"250-fake\r\n250-AUTH PLAIN\r\n250 8BITMIME\r\n",
            "STARTTLS" if starttls => {
                reader.get_mut().write_all(b"220 go ahead\r\n").await.ok()?;
                // The client sends nothing more before the handshake, so nothing is left in the buffer.
                return Some(reader.into_inner());
            }
            "AUTH" => {
                use base64::Engine as _;
                let arg = text.split(' ').nth(2).unwrap_or_default();
                let raw = base64::engine::general_purpose::STANDARD
                    .decode(arg)
                    .unwrap_or_default();
                let parts: Vec<String> = raw
                    .split(|b| *b == 0)
                    .map(|p| String::from_utf8_lossy(p).into_owned())
                    .collect();
                let creds = format!(
                    "{}:{}",
                    parts.get(1).cloned().unwrap_or_default(),
                    parts.get(2).cloned().unwrap_or_default()
                );
                let ok = creds == format!("user:{PASSWORD}");
                seen.auth.lock().unwrap().push(creds);
                if ok { b"235 ok\r\n" } else { b"535 no\r\n" }
            }
            "DATA" => {
                reader.get_mut().write_all(b"354 go\r\n").await.ok()?;
                let mut message = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).await.ok()? == 0 {
                        return None;
                    }
                    if line.trim_end() == "." {
                        break;
                    }
                    message.push_str(&line);
                }
                seen.messages.lock().unwrap().push(message);
                b"250 queued\r\n"
            }
            "QUIT" => {
                let _ = reader.get_mut().write_all(b"221 bye\r\n").await;
                return None;
            }
            "MAIL" | "RCPT" | "RSET" | "NOOP" => b"250 ok\r\n",
            _ => b"500 what\r\n",
        };
        reader.get_mut().write_all(reply).await.ok()?;
    }
}

/// An SMTP server on 127.0.0.1:0 speaking TLS the `mode` way, with `acceptor`'s certificate.
async fn tls_server(mode: Mode, acceptor: TlsAcceptor) -> (u16, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Seen::default();
    let s = seen.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let s = s.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let tcp = match mode {
                    Mode::Implicit => tcp,
                    Mode::StartTls => match session(tcp, true, false, true, &s).await {
                        Some(tcp) => tcp,
                        None => return,
                    },
                };
                match acceptor.accept(tcp).await {
                    Ok(tls) => {
                        let _ = session(tls, mode == Mode::Implicit, true, false, &s).await;
                    }
                    Err(e) => s.handshake_errors.lock().unwrap().push(e.to_string()),
                }
            });
        }
    });
    (port, seen)
}

fn settings(port: u16, encryption: &str, ca: Option<PathBuf>) -> MailSettings {
    let mut app = Settings::from_env();
    app.env = "testing".into();
    let mut s = MailSettings::from_env(&app);
    s.mailer = "smtp".into();
    s.host = "127.0.0.1".into();
    s.port = port;
    s.encryption = encryption.into();
    s.username = "user".into();
    s.password = PASSWORD.into();
    s.timeout = Duration::from_secs(2);
    s.tls_ca = ca;
    s
}

fn email() -> Email {
    Email::new(Envelope::new().to("ada@example.com").subject("Over TLS")).text("Sealed.")
}

fn from() -> Address {
    Address::new("app@example.com")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn implicit_tls_delivers_to_a_server_signed_by_mail_tls_ca() {
    let pki = pki();
    let (port, seen) = tls_server(Mode::Implicit, pki.acceptor.clone()).await;
    let transport = SmtpTransport::new(&settings(port, "tls", Some(pki.ca_pem.clone()))).unwrap();
    transport.send(&email(), &from()).await.unwrap();
    assert_eq!(
        seen.auth.lock().unwrap().as_slice(),
        [format!("user:{PASSWORD}")]
    );
    let commands = seen.commands.lock().unwrap().clone();
    assert!(!commands.is_empty());
    assert!(
        commands.iter().all(|(_, tls)| *tls),
        "every command over TLS: {commands:?}"
    );
    let message = seen.messages.lock().unwrap()[0].clone();
    assert!(message.contains("Subject: Over TLS"), "{message}");
    assert!(message.contains("Sealed."), "{message}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starttls_upgrades_before_the_login() {
    let pki = pki();
    let (port, seen) = tls_server(Mode::StartTls, pki.acceptor.clone()).await;
    let transport =
        SmtpTransport::new(&settings(port, "starttls", Some(pki.ca_pem.clone()))).unwrap();
    transport.send(&email(), &from()).await.unwrap();
    let commands = seen.commands.lock().unwrap().clone();
    let verbs: Vec<(&str, bool)> = commands.iter().map(|(v, t)| (v.as_str(), *t)).collect();
    assert_eq!(
        verbs.get(..3),
        Some(&[("EHLO", false), ("STARTTLS", false), ("EHLO", true)][..]),
        "{verbs:?}"
    );
    // The login and the mail only after the upgrade.
    for (verb, tls) in &verbs {
        if ["AUTH", "MAIL", "RCPT", "DATA"].contains(verb) {
            assert!(tls, "{verb} sent in the clear: {verbs:?}");
        }
    }
    assert_eq!(seen.messages.lock().unwrap().len(), 1);
    assert_eq!(
        seen.auth.lock().unwrap().as_slice(),
        [format!("user:{PASSWORD}")]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_untrusted_server_certificate_fails_fast_without_the_password() {
    let pki = pki();
    for (mode, encryption) in [(Mode::Implicit, "tls"), (Mode::StartTls, "starttls")] {
        let (port, seen) = tls_server(mode, pki.acceptor.clone()).await;
        // No MAIL_TLS_CA: the test CA is not trusted.
        let transport = SmtpTransport::new(&settings(port, encryption, None)).unwrap();
        let started = Instant::now();
        let err = transport
            .send(&email(), &from())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "{encryption}: {:?}",
            started.elapsed()
        );
        assert!(
            err.contains("the connection to the mail server 127.0.0.1 failed"),
            "{encryption}: {err}"
        );
        assert!(
            err.contains("certificate") || err.contains("UnknownIssuer"),
            "{encryption}: {err}"
        );
        assert!(!err.contains(PASSWORD), "{err}");
        assert!(
            seen.auth.lock().unwrap().is_empty(),
            "{encryption}: no login"
        );
        assert!(seen.messages.lock().unwrap().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ca_file_for_another_ca_does_not_help() {
    let server_pki = pki();
    let other = pki();
    let (port, seen) = tls_server(Mode::Implicit, server_pki.acceptor.clone()).await;
    let transport = SmtpTransport::new(&settings(port, "tls", Some(other.ca_pem.clone()))).unwrap();
    let err = transport
        .send(&email(), &from())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("failed"), "{err}");
    assert!(seen.messages.lock().unwrap().is_empty());
}
