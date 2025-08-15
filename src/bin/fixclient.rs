use std::io::{self, Write};
use std::net::TcpStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SOH: u8 = 0x01;

/// Create a simple FIX spike message with `MsgType=SPK`.
fn build_spike_message(spike_id: u32, who: &str, note: &str) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(256);

    // Standard Header
    push_fix_field(&mut out, 8, "FIX.4.2"); // BeginString
    push_fix_field(&mut out, 9, "000");     // BodyLength placeholder
    push_fix_field(&mut out, 35, "SPK");    // MsgType

    // Custom fields for spike
    push_fix_field(&mut out, 100, &spike_id.to_string());
    push_fix_field(&mut out, 101, who);
    push_fix_field(&mut out, 102, note);

    // Standard Trailer
    let ts = current_fix_timestamp();
    push_fix_field(&mut out, 52, &ts); // SendingTime

    // Calculate BodyLength
    let body_start = find_after_bodylen(&out).expect("body start");
    let body_len = out.len() - body_start;
    write_bodylen_in_place(&mut out, body_len).expect("rewrite 9=");

    // Checksum
    let cksum = checksum(&out);
    push_fix_field(&mut out, 10, &format!("{:03}", cksum));

    out
}

fn push_fix_field(buf: &mut Vec<u8>, tag: u32, value: &str) {
    write!(buf, "{}={}", tag, value).unwrap();
    buf.push(SOH);
}

fn current_fix_timestamp() -> String {
    let now = chrono::Utc::now();
    now.format("%Y%m%d-%H:%M:%S").to_string()
}

fn find_after_bodylen(buf: &[u8]) -> Option<usize> {
    let needle = b"9=";
    let mut i = 0;
    while i + 2 < buf.len() {
        if &buf[i..i + 2] == needle {
            let mut j = i + 2;
            while j < buf.len() && buf[j] != SOH {
                j += 1;
            }
            return if j < buf.len() { Some(j + 1) } else { None };
        }
        i += 1;
    }
    None
}

fn write_bodylen_in_place(buf: &mut Vec<u8>, len: usize) -> Option<()> {
    let needle = b"9=";
    let mut i = 0usize;
    while i + 2 <= buf.len() {
        if &buf[i..i + 2] == needle {
            let mut j = i + 2;
            while j < buf.len() && buf[j] != SOH {
                j += 1;
            }
            if j >= buf.len() {
                return None;
            }
            let digits = len.to_string();
            buf.splice(i + 2..j, digits.as_bytes().iter().copied());
            return Some(());
        }
        i += 1;
    }
    None
}

fn checksum(buf: &[u8]) -> u32 {
    buf.iter().fold(0u32, |acc, &b| acc + b as u32) % 256
}

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1:9878".into());
    let spike_id: u32 = args
        .next()
        .unwrap_or_else(|| "1".into())
        .parse()
        .expect("spike_id");
    let who = args.next().unwrap_or_else(|| "unknown".into());
    let note = args.next().unwrap_or_else(|| "no note".into());

    let msg = build_spike_message(spike_id, &who, &note);

    println!("Sending FIX message to {host}:\n{}", String::from_utf8_lossy(&msg));

    let mut stream = TcpStream::connect(host)?;
    stream.write_all(&msg)?;
    Ok(())
}