//! Byte stream abstraction over the modem's serial port, so the engine can be
//! driven by real hardware, a pseudo-terminal or the in-process simulator.

use crate::config::ModemConfig;
use async_trait::async_trait;
use std::io;
use tokio::io::{AsyncRead, AsyncWrite};

pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Stream for T {}

pub type BoxStream = Box<dyn Stream>;

/// Opens the connection to a modem.
#[async_trait]
pub trait Connector: Send + Sync {
    async fn open(&self, cfg: &ModemConfig) -> io::Result<BoxStream>;
}

/// Serial port connector for real hardware and pseudo-terminals.
pub struct SerialConnector;

#[async_trait]
impl Connector for SerialConnector {
    async fn open(&self, cfg: &ModemConfig) -> io::Result<BoxStream> {
        use tokio_serial::{
            DataBits, FlowControl, Parity, SerialPort, SerialPortBuilderExt, StopBits,
        };
        let builder = tokio_serial::new(&cfg.device, cfg.baud_rate)
            .data_bits(match cfg.data_bits {
                5 => DataBits::Five,
                6 => DataBits::Six,
                7 => DataBits::Seven,
                _ => DataBits::Eight,
            })
            .stop_bits(if cfg.stop_bits == 2 {
                StopBits::Two
            } else {
                StopBits::One
            })
            .parity(match cfg.parity.as_str() {
                "even" => Parity::Even,
                "odd" => Parity::Odd,
                _ => Parity::None,
            })
            .flow_control(match cfg.flow_control.as_str() {
                "hardware" => FlowControl::Hardware,
                "software" => FlowControl::Software,
                _ => FlowControl::None,
            });
        let mut port = builder.open_native_async().map_err(io::Error::other)?;
        // Exclusive access makes a second opener fail instead of silently
        // interleaving AT commands on the same line.
        let _ = port.set_exclusive(true);
        let _ = port.clear(tokio_serial::ClearBuffer::All);
        Ok(Box::new(port))
    }
}
