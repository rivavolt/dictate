use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub command: String,
    #[serde(default)]
    pub arg: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Response {
    pub fn ok(msg: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: Some(msg.into()),
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: Some(msg.into()),
        }
    }
}

pub fn bind(path: &Path) -> Result<UnixListener> {
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path).context("failed to bind IPC socket")
}

pub async fn send(path: &Path, req: &Request) -> Result<Response> {
    let stream = UnixStream::connect(path)
        .await
        .context("daemon not running (can't connect to socket)")?;
    let (reader, mut writer) = stream.into_split();

    let mut data = serde_json::to_vec(req)?;
    data.push(b'\n');
    writer.write_all(&data).await?;
    writer.shutdown().await?;

    let mut lines = BufReader::new(reader).lines();
    let line = lines
        .next_line()
        .await?
        .context("no response from daemon")?;
    let resp: Response = serde_json::from_str(&line)?;
    Ok(resp)
}
