use crate::config;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Parses the RTSP SETUP response to find the drone's server port.
fn get_server_port(response: &str) -> Option<u16> {
    // Find the "Transport:" header line.
    let transport_line = response
        .lines()
        .find(|line| line.starts_with("Transport:"))?;

    // Find the "server_port=" part within that line.
    let server_port_str = transport_line
        .split(';')
        .find(|part| part.trim().starts_with("server_port="))?
        .split('=')
        .nth(1)?
        .split('-') // Handles ranges like "53796-53797"
        .next()?; // Take the first port in the range

    server_port_str.parse::<u16>().ok()
}

/// Performs the initial TCP handshake with the drone using RTSP.
///
/// On success, returns a tuple containing:
/// - The video resolution (width, height).
/// - The server's chosen UDP port for the video stream.
pub fn perform(
    stream: &mut TcpStream,
    local_udp_port: u16,
) -> Result<u16, Box<dyn std::error::Error>> {
    println!("[Handshake] Starting TCP handshake...");
    let drone_addr = config::DEST_TCP_ADDR;

    // --- Step 1: Connect to the drone's RTSP port ---

    // --- Step 2: Send OPTIONS request ---
    let options_req = format!(
        "OPTIONS rtsp://{}/webcam RTSP/1.0\r\nCSeq: 1\r\nUser-Agent: ijkplayer\r\n\r\n",
        drone_addr
    );
    stream.write_all(options_req.as_bytes())?;
    println!("[Handshake] Sent OPTIONS request.");

    let mut buf = [0; 1024];
    stream.read(&mut buf)?; // Read the OPTIONS response

    // --- Step 3: Send DESCRIBE request ---
    let describe_req = format!(
        "DESCRIBE rtsp://{}/webcam RTSP/1.0\r\nAccept: application/sdp\r\nCSeq: 2\r\nUser-Agent: ijkplayer\r\n\r\n",
        drone_addr
    );
    stream.write_all(describe_req.as_bytes())?;
    println!("[Handshake] Sent DESCRIBE request.");

    let len = stream.read(&mut buf)?;
    let describe_response = String::from_utf8_lossy(&buf[..len]).to_string();
    println!("[Handshake] Decribe response: {}", describe_response);
    // --- Step 4: Send SETUP request ---
    // We get the local UDP port our app is bound to for video
    let setup_req = format!(
        "SETUP rtsp://{}/webcam/track0 RTSP/1.0\r\nTransport: RTP/AVP/UDP;unicast;client_port={}-{}\r\nCSeq: 3\r\nUser-Agent: ijkplayer\r\n\r\n",
        drone_addr,
        local_udp_port,
        local_udp_port + 1 // Request a range for RTP and RTCP
    );

    stream.write_all(setup_req.as_bytes())?;
    println!(
        "[Handshake] Sent SETUP request for UDP port {}.",
        local_udp_port
    );

    let len = stream.read(&mut buf)?;
    let setup_response = String::from_utf8_lossy(&buf[..len]).to_string();

    // Extract the server's port from the response
    let server_port = get_server_port(&setup_response)
        .ok_or("Could not parse setup response for server port.")?;

    println!(
        "[Handshake] Drone will stream video to our port {} from its port {}.",
        local_udp_port, server_port
    );

    // --- Step 5: Send PLAY request ---
    let play_req = format!(
        "PLAY rtsp://{}/webcam RTSP/1.0\r\nRange: npt=0.000-\r\nCSeq: 4\r\nUser-Agent: ijkplayer\r\nSession: {}\r\n\r\n",
        drone_addr,
        setup_response
            .lines()
            .find(|l| l.starts_with("Session:"))
            .unwrap_or("")
            .split_whitespace()
            .nth(1)
            .unwrap_or("")
    );
    stream.write_all(play_req.as_bytes())?;
    println!("[Handshake] Sent PLAY request.");
    stream.read(&mut buf)?; // Read PLAY response

    println!("[Handshake] Complete.");
    Ok(server_port)
}
