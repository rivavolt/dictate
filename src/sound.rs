use std::path::Path;
use std::process::Command;

pub fn play(sound_dir: &Path, name: &str) {
    let sound_file = sound_dir.join(format!("{}.wav", name));
    if sound_file.exists() {
        let _ = Command::new("pw-play").arg(&sound_file).spawn();
    }
}
