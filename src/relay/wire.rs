#![allow(dead_code)]

use crate::protocol::{Envelope, OutgoingMsg};
use crate::relay::crypto::Secretbox;
use tokio::sync::mpsc;

/// Fully-formed outbound WebSocket text frames queued for the relay writer task.
pub type OutTx = mpsc::Sender<String>;

/// Serialize + encrypt an OutgoingMsg into the relay `encrypted` envelope JSON.
///
/// `wrap_inner` binds the routing `clientId` inside the AEAD payload too
/// (#72), but only for clients that spoke the v2 wrapper first: a legacy
/// Shellular client parses the bare message shape and would drop a wrapped
/// one. The transport flips this per client after seeing a wrapped inbound.
pub fn encrypt_envelope(
    secretbox: &Secretbox,
    client_id: &str,
    msg: &OutgoingMsg,
    wrap_inner: bool,
) -> anyhow::Result<String> {
    let plaintext = if wrap_inner {
        serde_json::to_vec(&serde_json::json!({
            "clientId": client_id,
            "msg": msg,
        }))?
    } else {
        serde_json::to_vec(msg)?
    };
    let payload = secretbox.encrypt(plaintext)?;
    let envelope = Envelope::Encrypted {
        id: None,
        client_id: client_id.to_string(),
        nonce: payload.nonce,
        ciphertext: payload.ciphertext,
    };
    Ok(serde_json::to_string(&envelope)?)
}

/// Per-client context handed to feature handlers so they can stream encrypted
/// responses back to exactly one app client over the shared relay socket.
#[derive(Clone)]
pub struct ClientCtx {
    pub client_id: String,
    secretbox: Secretbox,
    out_tx: OutTx,
    wrap_inner: bool,
}

impl ClientCtx {
    pub fn new(client_id: String, secretbox: Secretbox, out_tx: OutTx, wrap_inner: bool) -> Self {
        Self {
            client_id,
            secretbox,
            out_tx,
            wrap_inner,
        }
    }

    /// Encrypt and enqueue an outgoing message for this client.
    pub async fn send(&self, msg: OutgoingMsg) {
        match encrypt_envelope(&self.secretbox, &self.client_id, &msg, self.wrap_inner) {
            Ok(frame) => {
                if let Err(e) = self.out_tx.send(frame).await {
                    tracing::warn!(client_id = %self.client_id, "relay out_tx closed, dropping frame: {e}");
                }
            }
            Err(e) => tracing::error!("Failed to encrypt outgoing message: {e}"),
        }
    }

    /// Send an error result correlated to a request id.
    pub async fn send_error(&self, resp_to: Option<String>, error: impl Into<String>) {
        self.send(OutgoingMsg::Error {
            id: None,
            resp_to,
            error: error.into(),
        })
        .await;
    }

    /// Send a result payload correlated to a request id.
    pub async fn send_result(&self, resp_to: Option<String>, data: serde_json::Value) {
        self.send(OutgoingMsg::Result {
            id: None,
            resp_to,
            data,
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_envelope_binds_client_id_inside_the_payload() {
        let secretbox = Secretbox::from_key_bytes([6; 32]);
        let json = encrypt_envelope(
            &secretbox,
            "client-a",
            &OutgoingMsg::Pong {
                id: Some("1".into()),
                resp_to: None,
            },
            true,
        )
        .unwrap();
        let Envelope::Encrypted {
            client_id,
            nonce,
            ciphertext,
            ..
        } = serde_json::from_str(&json).unwrap()
        else {
            panic!("expected an encrypted envelope");
        };
        assert_eq!(client_id, "client-a");
        // The plaintext carries the id a second time, inside the AEAD.
        let plaintext = secretbox.decrypt_parts(&nonce, &ciphertext).unwrap();
        let inner: serde_json::Value = serde_json::from_slice(&plaintext).unwrap();
        assert_eq!(inner["clientId"], "client-a");
        assert_eq!(inner["msg"]["type"], "pong");
    }

    #[test]
    fn bare_envelope_keeps_the_legacy_shape() {
        let secretbox = Secretbox::from_key_bytes([6; 32]);
        let json = encrypt_envelope(
            &secretbox,
            "client-a",
            &OutgoingMsg::Pong {
                id: Some("1".into()),
                resp_to: None,
            },
            false,
        )
        .unwrap();
        let Envelope::Encrypted {
            nonce, ciphertext, ..
        } = serde_json::from_str(&json).unwrap()
        else {
            panic!("expected an encrypted envelope");
        };
        let plaintext = secretbox.decrypt_parts(&nonce, &ciphertext).unwrap();
        // Legacy parsers read a bare message; the wrapper must not appear.
        let inner: serde_json::Value = serde_json::from_slice(&plaintext).unwrap();
        assert_eq!(inner["type"], "pong");
        assert!(inner.get("clientId").is_none());
        assert!(inner.get("msg").is_none());
    }
}
