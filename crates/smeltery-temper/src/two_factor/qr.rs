//! The enrolment QR code: an `otpauth://` URI drawn as an SVG made only of numbers (no text from the user goes into
//! the markup), handed to pages as a `data:` URI for an `<img>`.

use base64::Engine as _;
use qrcodegen::{QrCode, QrCodeEcc};
use smeltery_core::{Error, Result};

/// `otpauth://totp/<issuer>:<account>?secret=…&issuer=…&algorithm=SHA1&digits=6&period=30`, issuer and account
/// percent-encoded.
pub(crate) fn otpauth_uri(issuer: &str, account: &str, secret_base32: &str) -> String {
    let label = format!("{}:{}", encode(issuer), encode(account));
    format!(
        "otpauth://totp/{label}?secret={secret_base32}&issuer={}&algorithm=SHA1&digits={}&period={}",
        encode(issuer),
        super::totp::DIGITS,
        super::totp::PERIOD
    )
}

/// Percent-encode everything but RFC 3986 unreserved characters.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The QR code of `text` as SVG: one `<path>` of unit squares, a white background and a four-module quiet zone.
///
/// # Errors
/// The text is too long for a QR code.
pub(crate) fn svg(text: &str) -> Result<String> {
    let qr = QrCode::encode_text(text, QrCodeEcc::Medium)
        .map_err(|_| Error::internal("the two-factor URI is too long for a QR code"))?;
    let border = 4;
    let size = qr.size();
    let mut path = String::new();
    for y in 0..size {
        for x in 0..size {
            if qr.get_module(x, y) {
                path.push_str(&format!("M{},{}h1v1h-1z", x + border, y + border));
            }
        }
    }
    let full = size + 2 * border;
    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {full} {full}\" shape-rendering=\"crispEdges\">\
         <rect width=\"100%\" height=\"100%\" fill=\"#ffffff\"/><path d=\"{path}\" fill=\"#000000\"/></svg>"
    ))
}

/// `data:image/svg+xml;base64,…` of `svg`.
pub(crate) fn data_uri(svg: &str) -> String {
    format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(svg)
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_uri_escapes_the_issuer_and_the_account() {
        let uri = otpauth_uri("My App", "a+b@example.com", "ABC");
        assert_eq!(
            uri,
            "otpauth://totp/My%20App:a%2Bb%40example.com?secret=ABC&issuer=My%20App&algorithm=SHA1&digits=6&period=30"
        );
        let hostile = otpauth_uri("x", "\"><script>alert(1)</script>", "ABC");
        assert!(
            !hostile.contains('<') && !hostile.contains('"'),
            "{hostile}"
        );
    }

    #[test]
    fn the_svg_holds_only_numbers_and_fixed_markup() {
        let svg = svg(&otpauth_uri(
            "App",
            "\"><script>x</script>@example.com",
            "ABCDEF",
        ))
        .unwrap();
        assert!(svg.starts_with("<svg ") && svg.ends_with("</svg>"));
        assert!(!svg.contains("script") && !svg.contains("example"));
        let path = svg.split("d=\"").nth(1).unwrap().split('"').next().unwrap();
        assert!(
            path.bytes().all(|b| b"Mhvz,-0123456789".contains(&b)),
            "{path}"
        );
        assert!(data_uri(&svg).starts_with("data:image/svg+xml;base64,PHN2Zy"));
    }
}
