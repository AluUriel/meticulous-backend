//! Transport over which the read loop talks to the ESP32: real serial
//! (tokio-serial + reset pins) or the in-process emulator.

use std::io;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::SerialPortBuilderExt;

use crate::emulator::EmulatorTransport;
use crate::gpio::{reset_sequence, ResetPins};

/// Python `SerialConnection.BAUDRATE`.
pub const BAUDRATE: u32 = 115_200;
/// Python `uart.readline(timeout=0.5)`.
pub const READ_TIMEOUT: Duration = Duration::from_millis(500);

/// One read attempt.
#[derive(Debug)]
pub enum ReadOutcome {
    /// A full line (still carrying its `\r\n`).
    Line(Vec<u8>),
    /// No line within [`READ_TIMEOUT`] — run the healthcheck tick.
    Timeout,
    /// The port is gone.
    Error(io::Error),
}

/// The two transports; an enum (not a trait object) because async traits are
/// not dyn-compatible and there are exactly two implementations.
pub enum Transport {
    /// Real UART.
    Serial(SerialTransport),
    /// Fixture playback.
    Emulator(EmulatorTransport),
}

impl Transport {
    /// Read one line, waiting at most [`READ_TIMEOUT`].
    pub async fn read_line(&mut self) -> ReadOutcome {
        match self {
            Transport::Serial(serial) => serial.read_line().await,
            Transport::Emulator(emulator) => {
                match tokio::time::timeout(READ_TIMEOUT, emulator.read_line()).await {
                    Ok(line) => ReadOutcome::Line(line.into_bytes()),
                    Err(_) => ReadOutcome::Timeout,
                }
            }
        }
    }

    /// Write raw bytes to the ESP.
    pub async fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Transport::Serial(serial) => serial.port.write_all(bytes).await,
            Transport::Emulator(emulator) => {
                emulator.write(bytes);
                Ok(())
            }
        }
    }

    /// Reset the ESP, optionally into the bootloader
    /// (Python `SerialConnection.reset`).
    pub async fn reset(&mut self, bootloader: bool) {
        match self {
            Transport::Serial(serial) => {
                reset_sequence(serial.pins.as_mut(), bootloader).await;
            }
            Transport::Emulator(emulator) => emulator.reset(),
        }
    }
}

/// Real UART transport with the same line-buffering semantics as Python's
/// `Machine.ReadLine` (split on `\n`, timeout returns nothing).
pub struct SerialTransport {
    port: tokio_serial::SerialStream,
    pins: Box<dyn ResetPins>,
    buffer: Vec<u8>,
}

impl SerialTransport {
    /// Open `device` at the protocol baudrate.
    pub fn open(device: &str, pins: Box<dyn ResetPins>) -> io::Result<Self> {
        let port = tokio_serial::new(device, BAUDRATE)
            .open_native_async()
            .map_err(|e| io::Error::other(format!("cannot open {device}: {e}")))?;
        tracing::info!(device, "connected to ESP32");
        Ok(SerialTransport {
            port,
            pins,
            buffer: Vec::with_capacity(4096),
        })
    }

    async fn read_line(&mut self) -> ReadOutcome {
        if let Some(line) = self.take_buffered_line() {
            return ReadOutcome::Line(line);
        }
        let deadline = tokio::time::Instant::now() + READ_TIMEOUT;
        let mut chunk = [0u8; 2048];
        loop {
            let read = tokio::time::timeout_at(deadline, self.port.read(&mut chunk)).await;
            match read {
                Err(_) => return ReadOutcome::Timeout,
                Ok(Err(error)) => return ReadOutcome::Error(error),
                Ok(Ok(0)) => return ReadOutcome::Error(io::Error::other("serial port EOF")),
                Ok(Ok(n)) => {
                    self.buffer.extend_from_slice(chunk.get(..n).unwrap_or(&[]));
                    if let Some(line) = self.take_buffered_line() {
                        return ReadOutcome::Line(line);
                    }
                }
            }
        }
    }

    fn take_buffered_line(&mut self) -> Option<Vec<u8>> {
        let newline = self.buffer.iter().position(|b| *b == b'\n')?;
        let mut line: Vec<u8> = self.buffer.drain(..=newline).collect();
        line.shrink_to_fit();
        Some(line)
    }
}
