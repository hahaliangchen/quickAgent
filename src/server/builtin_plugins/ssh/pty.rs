use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use russh::{Channel, ChannelMsg, client};
use serde::Deserialize;

use super::client::Connection;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Control {
    Resize { cols: u16, rows: u16 },
}

pub(super) async fn open(connection: &Connection) -> Result<Channel<client::Msg>> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut channel = connection.channel_open_session().await?;
        channel
            .request_pty(true, "xterm-256color", 80, 24, 0, 0, &[])
            .await?;
        accepted(&mut channel, "PTY").await?;
        channel.request_shell(true).await?;
        accepted(&mut channel, "shell").await?;
        Ok(channel)
    })
    .await
    .context("SSH terminal setup timed out")?
}

async fn accepted(channel: &mut Channel<client::Msg>, request: &str) -> Result<()> {
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => return Ok(()),
            Some(ChannelMsg::Failure | ChannelMsg::Close) | None => {
                anyhow::bail!("SSH server refused {request}")
            }
            _ => (),
        }
    }
}

pub(super) async fn message(socket: &mut WebSocket, value: serde_json::Value) -> Result<()> {
    socket.send(Message::Text(value.to_string().into())).await?;
    Ok(())
}

pub(super) async fn relay(
    mut socket: WebSocket,
    channel: Channel<client::Msg>,
    connection: Connection,
) -> Result<()> {
    message(&mut socket, serde_json::json!({"type":"ready"})).await?;
    let (mut to_browser, mut from_browser) = socket.split();
    let (mut from_ssh, to_ssh) = channel.split();
    let read_remote = async {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
        heartbeat.tick().await;
        loop {
            tokio::select! {
                incoming = from_ssh.wait() => match incoming {
                    Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => to_browser.send(Message::Binary(data)).await?,
                    Some(ChannelMsg::ExitStatus { exit_status }) => to_browser.send(Message::Text(serde_json::json!({"type":"exit","code":exit_status}).to_string().into())).await?,
                    Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
                    _ => (),
                },
                _ = heartbeat.tick() => to_browser.send(Message::Ping(Default::default())).await?,
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    let read_browser = async {
        loop {
            let Some(value) =
                tokio::time::timeout(Duration::from_secs(65), from_browser.next()).await?
            else {
                break;
            };
            match value? {
                Message::Binary(bytes) => to_ssh.data_bytes(bytes).await?,
                Message::Text(text) => {
                    let Control::Resize { cols, rows } = serde_json::from_str(&text)?;
                    if (2..=512).contains(&cols) && (2..=256).contains(&rows) {
                        to_ssh.window_change(cols.into(), rows.into(), 0, 0).await?;
                    }
                }
                Message::Close(_) => break,
                _ => (),
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    let result = tokio::select! { result = read_remote => result, result = read_browser => result };
    let _ = tokio::time::timeout(Duration::from_secs(2), to_ssh.close()).await;
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        connection.disconnect(russh::Disconnect::ByApplication, "Terminal closed", ""),
    )
    .await;
    result
}
