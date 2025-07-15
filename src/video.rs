use crate::config;
use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const VIDEO_INIT_BYTES: [u8; 12] = [
    0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Runs the video forwarding logic on a pre-bound socket.
///
/// This function enters a loop to forward all received video packets
/// from the given socket to a local port for ffplay to consume.
pub fn run(
    local_addr: &str,
    rtcp_addr: &str,
    remote_addr: &str,
    fwd_addr: &str,
    running: Arc<AtomicBool>,
) -> std::io::Result<()> {
    let socket = UdpSocket::bind(local_addr)?;
    socket.send_to(&VIDEO_INIT_BYTES, remote_addr)?;
    // Set a short read timeout so the loop doesn't block forever.
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut buf = [0; 1452]; // Buffer for video data

    println!(
        "[Video] Forwarder started. Listening on port {}.",
        socket.local_addr()?.port()
    );

    let rtcp_socket = UdpSocket::bind(rtcp_addr)?;
    rtcp_socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut rtcp_buf = [0; 1024];
    println!(
        "[Video] Listening for control (RTCP) on port {}.",
        rtcp_socket.local_addr()?.port()
    );

    while running.load(Ordering::SeqCst) {
        match socket.recv_from(&mut buf) {
            Ok((num_bytes, _src_addr)) => {
                // Packet received, forward it directly to the ffplay address.
                socket.send_to(&buf[..num_bytes], fwd_addr)?;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Timeout is expected, just loop again.
                continue;
            }
            Err(e) => {
                eprintln!("[Video] Error receiving packet: {}", e);
                break;
            }
        }
        if let Ok((num_bytes, _)) = rtcp_socket.recv_from(&mut rtcp_buf) {
            // We successfully received the packet. We don't need to do anything with it.
            // This is enough to prevent the ICMP "Port Unreachable" error.
            println!(
                "[Video] Silently handled RTCP Sender Report ({} bytes).",
                num_bytes
            );
        }
    }

    println!("[Video] Forwarder shutting down.");
    Ok(())
}
