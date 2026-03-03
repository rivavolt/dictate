use anyhow::Result;
use evdev::{Device, EventType, InputEventKind, Key};
use futures::StreamExt;
use std::path::PathBuf;
use tokio::sync::mpsc;

pub async fn watch_key(key_code: u16, tx: mpsc::Sender<()>) -> Result<()> {
    let target_key = Key::new(key_code);
    let devices = enumerate_key_devices(target_key)?;

    if devices.is_empty() {
        anyhow::bail!(
            "no input device found with key {} capability",
            key_code
        );
    }

    tracing::info!(
        "watching {} device(s) for key code {}",
        devices.len(),
        key_code
    );

    let mut handles = Vec::new();
    for path in devices {
        let tx = tx.clone();
        handles.push(tokio::spawn(watch_device(path, target_key, tx)));
    }

    // If any device task ends, we keep running (others may still work).
    // If all end, the caller's recv will see the channel close.
    futures::future::join_all(handles).await;
    Ok(())
}

fn enumerate_key_devices(key: Key) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let readdir = std::fs::read_dir("/dev/input")
        .map_err(|e| anyhow::anyhow!("can't read /dev/input: {e}"))?;

    for entry in readdir.flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.starts_with("event") {
            continue;
        }
        if let Ok(dev) = Device::open(&path) {
            if dev
                .supported_keys()
                .map_or(false, |keys| keys.contains(key))
            {
                tracing::debug!("found device: {} ({})", path.display(), dev.name().unwrap_or("?"));
                paths.push(path);
            }
        }
    }
    Ok(paths)
}

async fn watch_device(path: PathBuf, key: Key, tx: mpsc::Sender<()>) {
    let dev = match Device::open(&path) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("failed to open {}: {e}", path.display());
            return;
        }
    };

    let mut stream = match dev.into_event_stream() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("failed to create stream for {}: {e}", path.display());
            return;
        }
    };

    while let Some(Ok(ev)) = stream.next().await {
        // value 1 = key press, ignore repeats (2) and releases (0)
        if ev.event_type() == EventType::KEY
            && ev.kind() == InputEventKind::Key(key)
            && ev.value() == 1
        {
            let _ = tx.send(()).await;
        }
    }

    tracing::debug!("device stream ended: {}", path.display());
}
