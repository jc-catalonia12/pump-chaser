//! WebSocket live snapshot — port of `api.py` `/ws`.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;

use crate::api::handlers;
use crate::AppState;

pub async fn ws_handler(State(state): State<Arc<AppState>>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(state, socket))
}

async fn handle_socket(state: Arc<AppState>, mut socket: WebSocket) {
    loop {
        let snapshot = handlers::live_snapshot(State(state.clone())).await.0;
        if socket
            .send(Message::Text(snapshot.to_string().into()))
            .await
            .is_err()
        {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
