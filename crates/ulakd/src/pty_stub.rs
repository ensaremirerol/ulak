//! No PTYs here (Windows): a node cannot stream; the type exists so the crate builds for the app.
use std::{fs::File, io};

#[derive(Debug)]
pub struct Pty;

fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "terminal streaming needs Linux or macOS",
    ))
}

impl Pty {
    pub fn spawn(
        _argv: &[String],
        _env: &[(&str, &str)],
        _cols: u16,
        _rows: u16,
    ) -> io::Result<Self> {
        unsupported()
    }
    pub fn reader(&self) -> io::Result<File> {
        unsupported()
    }
    pub fn write(&self, _data: &[u8]) -> io::Result<()> {
        unsupported()
    }
    pub fn resize(&self, _cols: u16, _rows: u16) -> io::Result<()> {
        unsupported()
    }
    pub fn alive(&mut self) -> bool {
        false
    }
}
