//! `/api/ws`: per-user event stream over the broadcast bus.

use std::collections::HashSet;

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use ytp_auth::{Permission, Principal, User};
use ytp_core::{CorpusEvent, Event, JobState};

use crate::auth::{origin_allowed, CurrentUser};
use crate::error::ApiError;
use crate::AppState;

const MAX_SUBSCRIPTIONS: usize = 256;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ServerMsg<'a> {
    Hello {
        user: &'a User,
        permissions: Vec<Permission>,
    },
    Pong,
    Job {
        id: &'a str,
        state: JobState,
        progress: f32,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<&'a str>,
    },
    Corpus {
        event: CorpusEvent,
        name: &'a str,
    },
    Error {
        message: String,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ClientMsg {
    Ping,
    Subscribe { topic: String },
    Unsubscribe { topic: String },
}

pub async fn handler(
    State(st): State<AppState>,
    CurrentUser(user): CurrentUser,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if !origin_allowed(&st, &headers) {
        return ApiError::forbidden("websocket origin not allowed").into_response();
    }
    match ws {
        Ok(ws) => ws.on_upgrade(move |socket| session(socket, st, user)),
        Err(e) => ApiError::bad_request(e.body_text()).into_response(),
    }
}

fn to_text(msg: &ServerMsg) -> Message {
    Message::Text(serde_json::to_string(msg).unwrap_or_default().into())
}

/// Whether `user` should see `event`, given their subscriptions.
fn visible<'a>(
    event: &'a Event,
    user: &Principal,
    subs: &HashSet<String>,
) -> Option<ServerMsg<'a>> {
    match event {
        Event::Job {
            id,
            owner,
            state,
            progress,
            message,
        } => {
            let allowed = *owner == user.user.id || user.has(Permission::JobsViewAll);
            let wanted = subs.contains(&format!("job:{id}")) || subs.contains("jobs");
            (allowed && wanted).then(|| ServerMsg::Job {
                id,
                state: *state,
                progress: *progress,
                message: message.as_deref(),
            })
        }
        Event::Corpus { event, name } => {
            user.has(Permission::CorpusRead)
                .then_some(ServerMsg::Corpus {
                    event: *event,
                    name,
                })
        }
    }
}

async fn session(socket: WebSocket, st: AppState, user: Principal) {
    let (mut tx, mut rx) = socket.split();
    let mut events = st.events.subscribe();
    let mut subs: HashSet<String> = HashSet::new();

    let hello = ServerMsg::Hello {
        user: &user.user,
        permissions: user.permission_list(),
    };
    if tx.send(to_text(&hello)).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            incoming = rx.next() => {
                let text = match incoming {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => continue,
                };
                let reply = match serde_json::from_str::<ClientMsg>(text.as_str()) {
                    Ok(ClientMsg::Ping) => Some(ServerMsg::Pong),
                    Ok(ClientMsg::Subscribe { topic }) => {
                        if topic.len() > 128 || subs.len() >= MAX_SUBSCRIPTIONS {
                            Some(ServerMsg::Error { message: "subscription refused".into() })
                        } else {
                            subs.insert(topic);
                            None
                        }
                    }
                    Ok(ClientMsg::Unsubscribe { topic }) => {
                        subs.remove(&topic);
                        None
                    }
                    Err(_) => Some(ServerMsg::Error { message: "unrecognised message".into() }),
                };
                if let Some(r) = reply {
                    if tx.send(to_text(&r)).await.is_err() {
                        break;
                    }
                }
            }
            ev = events.recv() => {
                match ev {
                    Ok(ev) => {
                        if let Some(msg) = visible(&ev, &user, &subs) {
                            if tx.send(to_text(&msg)).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
        }
    }
}
