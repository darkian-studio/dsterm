#![allow(dead_code)]

use crate::protocol::{Envelope, OutgoingMsg};
use crate::relay::crypto::Secretbox;
use tokio::sync::mpsc;

/// Fully-formed outbound WebSocket text frames queued for the relay writer task.
pub type OutTx = mpsc::Sender<String>;

/// Serialize + encrypt an OutgoingMsg into the relay `encrypted` envelope JSON.
pub fn encrypt_envelope(
    secretbox: &Secretbox,
    client_id: &str,
    msg: &OutgoingMsg,
) -> anyhow::Result<String> {
    let plaintext = serde_json::to_vec(msg)?;
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
}

impl ClientCtx {
    pub fn new(client_id: String, secretbox: Secretbox, out_tx: OutTx) -> Self {
        Self {
            client_id,
            secretbox,
            out_tx,
        }
    }

    /// Encrypt and enqueue an outgoing message for this client.
    pub async fn send(&self, msg: OutgoingMsg) {
        match encrypt_envelope(&self.secretbox, &self.client_id, &msg) {
            Ok(frame) => {
                if let Err(e) = self.out_tx.send(frame).await {
                    // FIX-073: log when relay writer is gone (was silent _ =)
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
