//! Web-push delivery + VAPID keypair generation.
//!
//! [`PushSender`] abstracts the act of encrypting and POSTing one push message
//! so [`crate::push`] can be unit-tested against a fake that records calls
//! instead of hitting a real push service. The production implementation is
//! [`WebPushSender`], built on the `web-push` crate (`hyper-client` feature,
//! no libcurl).
//!
//! VAPID keypairs are generated with the `p256` crate — the `web-push` crate
//! signs but does not generate. The private key is exported as the raw 32-byte
//! P-256 scalar (base64url, no pad), which is exactly what
//! `VapidSignatureBuilder::from_base64_no_sub` consumes; the public key is the
//! uncompressed 65-byte SEC1 point the browser uses as `applicationServerKey`.

use std::future::Future;
use std::pin::Pin;

use crate::push::{Subscription, VapidKeys};

/// The outcome of a single push delivery attempt.
#[derive(Debug)]
pub enum SendOutcome {
    /// Delivered (2xx from the push service).
    Ok,
    /// The endpoint is gone (404/410) — the caller should prune it.
    Gone,
    /// A transient failure (network, 5xx, auth) — keep the subscription.
    TransientError(String),
}

/// Sends one already-built push payload to one subscription. Object-safe via a
/// boxed future so [`crate::push::PushState`] can hold a `dyn PushSender`.
pub trait PushSender: Send + Sync + 'static {
    /// Encrypt `payload` for `sub`, sign with `keys`, and POST it.
    fn send<'a>(
        &'a self,
        keys: &'a VapidKeys,
        sub: &'a Subscription,
        payload: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = SendOutcome> + Send + 'a>>;
}

/// Generate a fresh VAPID keypair for `subject` (a `mailto:`/origin string).
///
/// Returns base64url(no-pad) encodings: the private key as the raw 32-byte
/// P-256 scalar, the public key as the uncompressed 65-byte SEC1 point.
pub fn generate_vapid_keys(subject: &str) -> anyhow::Result<VapidKeys> {
    use base64ct::{Base64UrlUnpadded, Encoding};
    use p256::SecretKey;
    use p256::elliptic_curve::sec1::ToEncodedPoint;

    let secret = SecretKey::random(&mut rand::rngs::OsRng);
    let priv_bytes: [u8; 32] = secret.to_bytes().into();
    let public = secret.public_key();
    let point = public.to_encoded_point(false); // uncompressed: 0x04 || X || Y
    Ok(VapidKeys {
        public_key: Base64UrlUnpadded::encode_string(point.as_bytes()),
        private_key: Base64UrlUnpadded::encode_string(&priv_bytes),
        subject: subject.to_string(),
    })
}

/// Production sender: encrypts with aes128gcm and signs with VAPID via the
/// `web-push` crate's hyper client (native-tls, no libcurl).
pub struct WebPushSender {
    client: web_push::HyperWebPushClient,
}

impl Default for WebPushSender {
    fn default() -> Self {
        Self::new()
    }
}

impl WebPushSender {
    /// Build a sender with a reusable hyper client.
    pub fn new() -> Self {
        Self {
            client: web_push::HyperWebPushClient::new(),
        }
    }
}

impl PushSender for WebPushSender {
    fn send<'a>(
        &'a self,
        keys: &'a VapidKeys,
        sub: &'a Subscription,
        payload: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = SendOutcome> + Send + 'a>> {
        Box::pin(async move {
            use web_push::{
                ContentEncoding, SubscriptionInfo, VapidSignatureBuilder, WebPushClient,
                WebPushError, WebPushMessageBuilder,
            };

            let info = SubscriptionInfo::new(&sub.endpoint, &sub.p256dh, &sub.auth);

            let sig = match VapidSignatureBuilder::from_base64_no_sub(&keys.private_key) {
                Ok(partial) => {
                    let mut b = partial.add_sub_info(&info);
                    b.add_claim("sub", keys.subject.clone());
                    match b.build() {
                        Ok(s) => s,
                        Err(e) => return SendOutcome::TransientError(format!("vapid build: {e}")),
                    }
                }
                Err(e) => return SendOutcome::TransientError(format!("vapid key: {e}")),
            };

            let mut builder = WebPushMessageBuilder::new(&info);
            builder.set_payload(ContentEncoding::Aes128Gcm, payload);
            builder.set_vapid_signature(sig);
            builder.set_ttl(86_400);

            let message = match builder.build() {
                Ok(m) => m,
                Err(e) => return SendOutcome::TransientError(format!("message build: {e}")),
            };

            match self.client.send(message).await {
                Ok(()) => SendOutcome::Ok,
                Err(WebPushError::EndpointNotFound(_) | WebPushError::EndpointNotValid(_)) => {
                    SendOutcome::Gone
                }
                Err(e) => SendOutcome::TransientError(e.to_string()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_have_expected_shapes() {
        use base64ct::{Base64UrlUnpadded, Encoding};
        let keys = generate_vapid_keys("mailto:test@example.com").unwrap();
        assert_eq!(keys.subject, "mailto:test@example.com");

        // Private key is the raw 32-byte scalar.
        let priv_bytes = Base64UrlUnpadded::decode_vec(&keys.private_key).unwrap();
        assert_eq!(
            priv_bytes.len(),
            32,
            "VAPID private key is a 32-byte scalar"
        );

        // Public key is the uncompressed 65-byte SEC1 point (0x04 prefix).
        let pub_bytes = Base64UrlUnpadded::decode_vec(&keys.public_key).unwrap();
        assert_eq!(pub_bytes.len(), 65, "applicationServerKey is 65 bytes");
        assert_eq!(
            pub_bytes[0], 0x04,
            "uncompressed SEC1 point starts with 0x04"
        );
    }

    #[test]
    fn keys_round_trip_into_a_vapid_signature() {
        // The generated private key must be loadable by the web-push signer —
        // this catches any format drift between generation and signing.
        let keys = generate_vapid_keys("mailto:test@example.com").unwrap();
        let info = web_push::SubscriptionInfo::new("https://example.com/ep", "BNcRrandom", "auth0");
        let partial =
            web_push::VapidSignatureBuilder::from_base64_no_sub(&keys.private_key).unwrap();
        let mut b = partial.add_sub_info(&info);
        b.add_claim("sub", keys.subject.clone());
        assert!(b.build().is_ok(), "generated key must sign a VAPID JWT");
    }
}
