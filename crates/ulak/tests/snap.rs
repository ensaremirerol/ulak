//! Runs the real binary headless and checks it writes a PNG per panel scene into tests/snapshots/.
use std::process::Command;

#[test]
fn snap_writes_a_png_per_scene() {
    for scene in ["mux", "questions", "spawn", "mailbox", "devices", "busy"] {
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("tests/snapshots/{scene}.png"));
        let _ = std::fs::remove_file(&out);
        let st = Command::new(env!("CARGO_BIN_EXE_ulak"))
            .args(["--snap", out.to_str().unwrap(), "--scene", scene])
            .status()
            .unwrap();
        assert!(st.success(), "{scene}");
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "{scene}");
    }
}
