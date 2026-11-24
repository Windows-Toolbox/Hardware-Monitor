//! The smart plug: the measured power of everything on it, over the home network – no cloud, no Bluetooth.
//!
//! Anker SOLIX Smart Plug Gen 2 (A17X8, product code "QNA" in its serial number): Modbus TCP on port 502, once it is
//! switched on in the Anker app (device → gear → "Three-Party Control Settings" → "Modbus TCP"). The registers are
//! those of Anker's own Home Assistant integration (github.com/anker-charging/ha-anker-solix-official, its config
//! for the plug), unit id 1, function 04 (input registers), only read – 30047, the plug's on/off switch, is never
//! touched. The plug announces itself by mDNS (`_anker_power._udp.local`, with `pn=` and `sn=`), so its address
//! is found without typing it. It measures anew about every 0.4 s; it answers in a few milliseconds.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::time::Duration;

const PORT: u16 = 502;
const UNIT: u8 = 1;
const TIMEOUT: Duration = Duration::from_millis(1500);
/// Registers 30029 (switch) … 30037 (temperature).
const VALUES: u16 = 30029;
const SERIAL: u16 = 30005;
const MODEL: u16 = 32768;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Values {
    pub on: bool,
    pub watts: f64,
    pub volts: f64,
    pub amps: f64,
    /// The plug's own counter since it was set up.
    pub kwh: f64,
    pub celsius: f64,
}

/// What the plug says about itself.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Info {
    pub model: String,
    pub serial: String,
}

impl Info {
    /// "Anker SOLIX Smart Plug Gen 2" from the serial number's product code (characters 4–6 of 16, Anker's rule).
    pub fn name(&self) -> String {
        match self.serial.get(3..6) {
            Some("QNA") if self.serial.len() == 16 => "Anker SOLIX Smart Plug Gen 2".into(),
            _ => "Anker SOLIX Smart Plug".into(),
        }
    }
}

/// One connection to the plug, kept open between readings and opened again after an error.
pub struct Plug {
    pub address: Ipv4Addr,
    stream: Option<TcpStream>,
    transaction: u16,
}

impl Plug {
    pub fn new(address: Ipv4Addr) -> Plug {
        Plug { address, stream: None, transaction: 0 }
    }

    fn registers(&mut self, start: u16, count: u16) -> std::io::Result<Vec<u16>> {
        let result = self.try_registers(start, count);
        if result.is_err() {
            self.stream = None;
        }
        result
    }

    fn try_registers(&mut self, start: u16, count: u16) -> std::io::Result<Vec<u16>> {
        if self.stream.is_none() {
            let s = TcpStream::connect_timeout(&SocketAddr::V4(SocketAddrV4::new(self.address, PORT)), TIMEOUT)?;
            s.set_read_timeout(Some(TIMEOUT))?;
            s.set_write_timeout(Some(TIMEOUT))?;
            s.set_nodelay(true)?;
            self.stream = Some(s);
        }
        self.transaction = self.transaction.wrapping_add(1);
        let s = self.stream.as_mut().expect("just opened");
        s.write_all(&request(self.transaction, start, count))?;
        let mut header = [0u8; 7];
        s.read_exact(&mut header)?;
        let length = u16::from_be_bytes([header[4], header[5]]) as usize;
        if !(2..=260).contains(&length) {
            return Err(std::io::Error::other("not a Modbus answer"));
        }
        let mut body = vec![0u8; length - 1];
        s.read_exact(&mut body)?;
        if u16::from_be_bytes([header[0], header[1]]) != self.transaction {
            return Err(std::io::Error::other("an answer to another question"));
        }
        parse(&body, count)
    }

    pub fn values(&mut self) -> std::io::Result<Values> {
        let r = self.registers(VALUES, 9)?;
        Ok(Values {
            on: r[0] == 1,
            watts: r[1] as f64 / 10.0,
            volts: r[2] as f64 / 10.0,
            amps: r[3] as f64 / 100.0,
            kwh: ((r[4] as u32) << 16 | r[5] as u32) as f64 / 1000.0,
            celsius: r[8] as i16 as f64 / 10.0,
        })
    }

    pub fn info(&mut self) -> std::io::Result<Info> {
        let model = self.registers(MODEL, 5)?;
        let serial = self.registers(SERIAL, 12)?;
        Ok(Info { model: text(&model), serial: text(&serial) })
    }
}

/// "Read input registers" (function 04).
fn request(transaction: u16, start: u16, count: u16) -> Vec<u8> {
    let mut r = Vec::with_capacity(12);
    r.extend_from_slice(&transaction.to_be_bytes());
    r.extend_from_slice(&[0, 0, 0, 6, UNIT, 4]);
    r.extend_from_slice(&start.to_be_bytes());
    r.extend_from_slice(&count.to_be_bytes());
    r
}

/// The part after the header: function, byte count, the registers – or an exception.
fn parse(body: &[u8], count: u16) -> std::io::Result<Vec<u16>> {
    match body {
        [f, code, ..] if f & 0x80 != 0 => Err(std::io::Error::other(format!("the plug refused (Modbus exception {code})"))),
        [4, n, data @ ..] if *n as usize == count as usize * 2 && data.len() >= *n as usize => {
            Ok(data[..*n as usize].chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect())
        }
        _ => Err(std::io::Error::other("an unexpected Modbus answer")),
    }
}

/// Two ASCII characters per register, zeros at the end.
fn text(regs: &[u16]) -> String {
    let bytes: Vec<u8> = regs.iter().flat_map(|r| r.to_be_bytes()).take_while(|b| *b != 0).collect();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

/// Looks for an Anker plug on the home network: one mDNS question, the answers of up to `wait`. The address and
/// what it announces (`pn`, `sn`) of the first plug that answers.
pub fn discover(wait: Duration) -> Option<(Ipv4Addr, Info)> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.set_read_timeout(Some(Duration::from_millis(300))).ok()?;
    let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for part in ["_anker_power", "_udp", "local"] {
        q.push(part.len() as u8);
        q.extend_from_slice(part.as_bytes());
    }
    // PTR, class IN with the "unicast answer" bit: the answer comes straight back to this socket.
    q.extend_from_slice(&[0, 0, 12, 0x80, 1]);
    socket.send_to(&q, (Ipv4Addr::new(224, 0, 0, 251), 5353)).ok()?;
    let until = std::time::Instant::now() + wait;
    let mut buf = [0u8; 4096];
    while std::time::Instant::now() < until {
        let Ok((n, SocketAddr::V4(from))) = socket.recv_from(&mut buf) else { continue };
        let data = &buf[..n];
        let field = |key: &str| -> Option<String> {
            let key = key.as_bytes();
            let at = data.windows(key.len()).position(|w| w == key)?;
            // A TXT string: its length byte stands right before it.
            let len = *data.get(at.checked_sub(1)?)? as usize;
            let value = data.get(at + key.len()..at + len)?;
            Some(String::from_utf8_lossy(value).to_string())
        };
        if let (Some(model), Some(serial)) = (field("pn="), field("sn=")) {
            return Some((*from.ip(), Info { model, serial }));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_and_its_answer() {
        assert_eq!(request(7, 30029, 9), vec![0, 7, 0, 0, 0, 6, 1, 4, 0x75, 0x4d, 0, 9]);
        let regs = parse(&[4, 4, 0x09, 0x92, 0x08, 0xf8], 2).unwrap();
        assert_eq!(regs, vec![2450, 2296]);
        assert!(parse(&[0x84, 2], 2).is_err(), "an exception");
        assert!(parse(&[4, 2, 0, 1], 2).is_err(), "too short");
        assert_eq!(text(&[0x4131, 0x3758, 0x3800, 0, 0]), "A17X8");
        let info = Info { model: "A17X8".into(), serial: "AYTQNA0G23301098".into() };
        assert_eq!(info.name(), "Anker SOLIX Smart Plug Gen 2");
    }
}
