use std::io::Write;
use std::process::Command;

pub fn type_text(text: &str) {
    if !text.is_empty() {
        let _ = Command::new("wtype")
            .arg("--")
            .arg(format!("{} ", text))
            .status();
    }
}

pub fn copy_to_clipboard(text: &str) {
    if let Ok(mut child) = Command::new("wl-copy")
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}
