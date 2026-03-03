use ksni::{self, Tray, TrayMethods};
use tokio::sync::mpsc;

pub struct DictateTray {
    recording: bool,
    toggle_tx: mpsc::Sender<()>,
}

impl DictateTray {
    pub fn set_recording(&mut self, recording: bool) {
        self.recording = recording;
    }
}

impl Tray for DictateTray {
    fn id(&self) -> String {
        "dictate".into()
    }

    fn title(&self) -> String {
        if self.recording {
            "Dictate (recording)".into()
        } else {
            "Dictate".into()
        }
    }

    fn icon_name(&self) -> String {
        "audio-input-microphone".into()
    }

    fn status(&self) -> ksni::Status {
        if self.recording {
            ksni::Status::Active
        } else {
            ksni::Status::Passive
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.toggle_tx.try_send(());
    }
}

pub async fn spawn(toggle_tx: mpsc::Sender<()>) -> anyhow::Result<ksni::Handle<DictateTray>> {
    let tray = DictateTray {
        recording: false,
        toggle_tx,
    };
    let handle = tray.spawn().await?;
    Ok(handle)
}
