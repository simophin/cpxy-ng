use anyhow::Context;
use axum::Router;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use serde::Serialize;
use std::borrow::Cow;
use tokio::net::TcpListener;
use tokio::sync::broadcast::Receiver;

#[derive(Serialize, Clone)]
#[serde(tag = "type")]
pub enum OutboundEvent {
    Connected {
        host: String,
        outbound: Cow<'static, str>,
        delay_mills: usize,
        request_time_mills: u64,
    },
    Error {
        host: String,
        outbound: Cow<'static, str>,
        delay_mills: usize,
        request_time_mills: u64,
        error: String,
    },
}

pub struct StatsProvider {
    pub events: Receiver<OutboundEvent>,
}

struct RouteState {
    events: Receiver<OutboundEvent>,
}

impl Clone for RouteState {
    fn clone(&self) -> Self {
        Self {
            events: self.events.resubscribe(),
        }
    }
}

pub async fn serve_stats(provider: StatsProvider, listener: TcpListener) -> anyhow::Result<()> {
    let StatsProvider { events } = provider;

    let router = Router::new()
        .route("/events", get(write_events))
        .with_state(RouteState { events });

    axum::serve(listener, router).await.context("server error")
}

async fn write_events(state: State<RouteState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(async move |socket| {
        if let Err(err) = handle_websocket(state.events.resubscribe(), socket).await {
            tracing::error!(?err, "WebSocket error");
        }
    })
}

async fn handle_websocket(
    mut events: Receiver<OutboundEvent>,
    mut socket: WebSocket,
) -> anyhow::Result<()> {
    loop {
        let event = events.recv().await?;
        socket
            .send(Message::Text(serde_json::to_string(&event)?.into()))
            .await?;
    }
}
