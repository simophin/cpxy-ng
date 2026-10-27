//! Reconnect/retry uses a fresh challenge, token, nonce and random HTTP path.
use super::http_io as wire;
use crate::{
    v2::http::{self, Bootstrap, BootstrapReply, Codec, Command, Kind, Reply, Rpc},
    worker::SocketProvider,
};
use serde_json::{json, Value};
use std::{io, net::SocketAddr, sync::Arc, time::Instant};
use uuid::Uuid;

pub struct Client {
    codec: Codec,
    pub peer: SocketAddr,
    provider: Arc<dyn SocketProvider>,
    pub client: Uuid,
    bootstrap: Option<(BootstrapReply, Instant)>,
}
fn invalid() -> io::Error {
    io::Error::other("invalid authenticated control response")
}
impl Client {
    pub fn new(
        psk: &[u8; 32],
        identity: &str,
        peer: SocketAddr,
        provider: Arc<dyn SocketProvider>,
        client: Uuid,
    ) -> io::Result<Self> {
        Ok(Self {
            codec: Codec::new(psk, identity).ok_or_else(invalid)?,
            peer,
            provider,
            client,
            bootstrap: None,
        })
    }
    fn post(&self, body: &[u8]) -> io::Result<Vec<u8>> {
        wire::post(
            self.provider.control_socket(self.peer)?,
            &self.peer.to_string(),
            body,
        )
    }
    fn bootstrap(&mut self) -> io::Result<()> {
        let token = http::hex(&http::random::<16>());
        let request = self
            .codec
            .seal(
                Kind::Bootstrap,
                &Bootstrap {
                    token: token.clone(),
                },
            )
            .ok_or_else(invalid)?;
        let started = Instant::now();
        let body = self.post(&request)?;
        let reply: BootstrapReply = self
            .codec
            .open(Kind::BootstrapReply, &body)
            .ok_or_else(invalid)?;
        if reply.token != token
            || reply.request_hash != http::hash(&request)
            || http::unhex::<16>(&reply.generation).is_none()
            || http::unhex::<16>(&reply.challenge).is_none()
            || reply.valid_for_ms != http::CHALLENGE_MS
        {
            return Err(invalid());
        }
        self.bootstrap = Some((reply, started));
        Ok(())
    }
    /// A caller retries with the same Operation value after an unknown outcome.
    pub fn operation(
        &mut self,
        method: &str,
        extra: serde_json::Map<String, Value>,
    ) -> io::Result<Pending> {
        self.bootstrap()?;
        let (b, _) = self.bootstrap.as_ref().unwrap();
        let mut params = extra;
        params.insert("client_id".into(), json!(self.client));
        params.insert("operation_id".into(), json!(Uuid::new_v4()));
        params.insert("generation".into(), json!(b.generation));
        params.insert("issued_ms".into(), json!(b.server_age_ms));
        Ok(Pending {
            method: method.into(),
            params,
        })
    }
    pub fn execute(&mut self, operation: &Pending) -> io::Result<Value> {
        // Every attempt uses a new bootstrap, even if the preceding HTTP outcome
        // is unknown. The encrypted operation parameters remain identical.
        self.bootstrap()?;
        let (b, _) = self.bootstrap.as_ref().unwrap();
        if operation.params.get("generation").and_then(Value::as_str) != Some(&b.generation) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "server generation changed",
            ));
        }
        let token = http::hex(&http::random::<16>());
        let rpc = Rpc {
            jsonrpc: "2.0".into(),
            id: Uuid::new_v4().to_string(),
            method: operation.method.clone(),
            params: operation.params.clone(),
        };
        let id = rpc.id.clone();
        let command = Command {
            token: token.clone(),
            generation: b.generation.clone(),
            challenge: b.challenge.clone(),
            rpc,
        };
        let request = self
            .codec
            .seal(Kind::Command, &command)
            .ok_or_else(invalid)?;
        let body = self.post(&request)?;
        let reply: Reply = self.codec.open(Kind::Reply, &body).ok_or_else(invalid)?;
        if reply.token != token
            || reply.request_hash != http::hash(&request)
            || reply.rpc["jsonrpc"] != "2.0"
            || reply.rpc["id"] != id
        {
            return Err(invalid());
        }
        let fields = reply.rpc.as_object().ok_or_else(invalid)?;
        if fields.len() != 3 || (fields.contains_key("result") == fields.contains_key("error")) {
            return Err(invalid());
        }
        if reply.rpc.get("error").is_some() {
            let code = reply.rpc["error"]["code"].as_i64().ok_or_else(invalid)?;
            return Err(io::Error::new(
                if code == -32001 {
                    io::ErrorKind::ConnectionReset
                } else {
                    io::ErrorKind::Other
                },
                format!("authenticated control error {code}"),
            ));
        }
        reply.rpc.get("result").cloned().ok_or_else(invalid)
    }
    pub fn call(
        &mut self,
        method: &str,
        extra: serde_json::Map<String, Value>,
    ) -> io::Result<Value> {
        let operation = self.operation(method, extra)?;
        let mut error = None;
        for _ in 0..3 {
            match self.execute(&operation) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    if e.kind() == io::ErrorKind::ConnectionReset {
                        return Err(e);
                    }
                    error = Some(e);
                }
            }
        }
        Err(error.unwrap())
    }
}
/// No Debug: operation contents may later grow sensitive fields.
#[derive(Clone)]
pub struct Pending {
    method: String,
    params: serde_json::Map<String, Value>,
}
