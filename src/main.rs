pub mod handshake;
pub mod video;

use std::io::{Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

// --- Configuration ---
mod config {
    use std::time::Duration;
    pub const SOURCE_ADDR: &str = "0.0.0.0:58737";
    pub const DEST_ADDR: &str = "192.168.1.1:7099";
    pub const DEST_TCP_ADDR: &str = "192.168.1.1:7070";
    pub const DEST_HOST: &str = "192.168.1.1";
    pub const LOCAL_HOST: &str = "192.168.1.100";
    pub const VIDEO_FWD_ADDR: &str = "127.0.0.1:9090";
    pub const LOCAL_VIDEO_PORT: u16 = 8768;
    pub const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(100);
    pub const MOVEMENT_COMMAND_DUR: Duration = Duration::from_millis(2000);
}

// --- Data Structures ---

pub type Heartbeat = [u8; 9];

#[derive(Debug, Clone)]
pub struct Command {
    pub payload: Heartbeat,
    pub duration: Duration,
    pub priority: u32,
}

#[derive(Debug, Clone)]
struct ActiveCommand {
    payload: Heartbeat,
    end_time: Instant,
    priority: u32,
}

pub const fn create_heartbeat(ctrl_bits: ControlBits) -> Heartbeat {
    let verif_bit: u8 = ctrl_bits[0] ^ ctrl_bits[1] ^ ctrl_bits[2] ^ ctrl_bits[3] ^ ctrl_bits[4];
    [
        0x03,
        0x66,
        ctrl_bits[0],
        ctrl_bits[1],
        ctrl_bits[2],
        ctrl_bits[3],
        ctrl_bits[4],
        verif_bit,
        0x99,
    ]
}

pub type ControlBits = [u8; 5];

const DEFAULT_CTRL_BITS: ControlBits = [0x80, 0x80, 0x80, 0x80, 0x00];

const CTRL_LAUNCH: u8 = 0x01;
const CTRL_LAND: u8 = 0x02;
const CTRL_AUX: u8 = 0x03;
const CTRL_PANIC: u8 = 0x04;

/// Listens for keyboard input and sends commands.
fn run_keyboard_listener(tx: Sender<Command>) {
    println!("--> Press SPACE for a high-priority command (2s).");
    println!("--> Press 'c' for a medium-priority command (5s).");
    println!("--> Press 'd' for a low-priority command (3s).");
    println!("--> Press Ctrl+C to exit.");

    rdev::listen(move |event| {
        let mut ctrl_bits: ControlBits = DEFAULT_CTRL_BITS;
        match event.event_type {
            rdev::EventType::KeyPress(rdev::Key::Space) => {
                ctrl_bits[4] = CTRL_LAUNCH;
            }

            rdev::EventType::KeyPress(rdev::Key::KeyL) => {
                ctrl_bits[4] = CTRL_LAND;
            }
            rdev::EventType::KeyPress(rdev::Key::KeyO) => {
                ctrl_bits[4] = CTRL_AUX;
            }
            rdev::EventType::KeyPress(rdev::Key::KeyP) => {
                ctrl_bits[4] = CTRL_PANIC;
            }
            rdev::EventType::KeyPress(rdev::Key::KeyA) => {
                ctrl_bits[0] -= 0x1d;
            }
            rdev::EventType::KeyPress(rdev::Key::KeyD) => {
                ctrl_bits[0] += 0x1d;
            }
            rdev::EventType::KeyPress(rdev::Key::KeyW) => {
                ctrl_bits[1] += 0x1d;
            }
            rdev::EventType::KeyPress(rdev::Key::KeyS) => {
                ctrl_bits[1] -= 0x1d;
            }
            rdev::EventType::KeyPress(rdev::Key::KeyJ) => {
                ctrl_bits[1] += 0x1d;
            }
            rdev::EventType::KeyPress(rdev::Key::KeyK) => {
                ctrl_bits[1] -= 0x1d;
            }
            rdev::EventType::KeyPress(rdev::Key::KeyQ) => {
                ctrl_bits[3] += 0x1d;
            }
            rdev::EventType::KeyPress(rdev::Key::KeyE) => {
                ctrl_bits[3] -= 0x1d;
            }
            _ => (),
        }

        if ctrl_bits != DEFAULT_CTRL_BITS {
            let packet = create_heartbeat(ctrl_bits);
            // Create an empty string to build into
            let mut hex_string = String::new();

            for (i, byte) in packet.iter().enumerate() {
                // Add the formatted byte to the string
                hex_string.push_str(&format!("{:02X}", byte));

                // Add a space, but not after the very last byte
                if i < packet.len() - 1 {
                    hex_string.push(' ');
                }
            }

            println!("{}", hex_string);
            let _ = tx.send(Command {
                payload: packet,
                duration: config::MOVEMENT_COMMAND_DUR,
                priority: 0,
            });
        }
    })
    .expect("Failed to listen to keyboard events");
}

/// Manages the main network loop, checking for an exit signal.
fn run_network_loop(
    socket: UdpSocket,
    rx: Receiver<Command>,
    standby_payload: [u8; 9],
    running: Arc<AtomicBool>,
) -> std::io::Result<()> {
    let mut current_command: Option<ActiveCommand> = None;

    let mut small_hb_time: Instant = Instant::now();

    // The loop now checks the `running` flag on each iteration.
    while running.load(Ordering::SeqCst) {
        if let Ok(new_cmd) = rx.try_recv() {
            let should_override = match &current_command {
                Some(active_cmd) => {
                    if new_cmd.priority >= active_cmd.priority {
                        println!(
                            "[System] New command (prio {}) overrides active command (prio {}).",
                            new_cmd.priority, active_cmd.priority
                        );
                        true
                    } else {
                        println!(
                            "[System] New command (prio {}) ignored; active command has higher or equal priority (prio {}).",
                            new_cmd.priority, active_cmd.priority
                        );
                        false
                    }
                }
                None => {
                    println!(
                        "[System] No active command. Starting new command (prio {}).",
                        new_cmd.priority
                    );
                    true
                }
            };

            if should_override {
                current_command = Some(ActiveCommand {
                    payload: new_cmd.payload,
                    end_time: Instant::now() + new_cmd.duration,
                    priority: new_cmd.priority,
                });
            }
        }

        if let Some(active_cmd) = &current_command {
            if Instant::now() >= active_cmd.end_time {
                println!(
                    "\n[System] Command (prio {}) expired. Reverting to standby.",
                    active_cmd.priority
                );
                current_command = None;
            }
        }

        if let Some(active_cmd) = &current_command {
            socket.send_to(&active_cmd.payload, config::DEST_ADDR)?;
            print!(
                "Sending active command (prio {})...  \r",
                active_cmd.priority
            );
        } else {
            socket.send_to(&standby_payload, config::DEST_ADDR)?;
            print!("Sending standby packet...            \r");
        }

        if Instant::now().duration_since(small_hb_time) >= Duration::from_secs(1) {
            socket.send_to(&[0x01, 0x01], config::DEST_ADDR)?;
            small_hb_time = Instant::now();
        }

        use std::io::Write;
        std::io::stdout().flush()?;
        thread::sleep(config::HEARTBEAT_INTERVAL);
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (tx, rx): (Sender<Command>, Receiver<Command>) = mpsc::channel();
    let standby_payload: Heartbeat = create_heartbeat(DEFAULT_CTRL_BITS);
    // Create the shared atomic boolean to signal exit.
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // Set the Ctrl+C handler. When triggered, it sets `running` to false.
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    println!("Starting UDP packet sender...");
    println!("--> Sending to {}", config::DEST_ADDR);

    //    thread::spawn(move || run_keyboard_listener(tx));
    let socket = UdpSocket::bind(config::SOURCE_ADDR)?;

    let mut tcp_stream =
        TcpStream::connect_timeout(&config::DEST_TCP_ADDR.parse()?, Duration::from_secs(5))?;
    tcp_stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    println!(
        "[Handshake] TCP connection established with {}.",
        config::DEST_TCP_ADDR,
    );

    //let video_port = handshake::perform(&mut tcp_stream, config::LOCAL_VIDEO_PORT)?;

    //let tcp_thread = {
    //  let running_tcp = running.clone();
    //  thread::spawn(move || run_tcp_keepalive(tcp_stream, running_tcp))
    //};

    std::thread::spawn(move || {
        println!(
            "TCP handshake establihsed video streaming at port {}.",
            config::LOCAL_VIDEO_PORT,
        );
        // Pass the `running` flag to the network loop.
        if let Err(e) = run_network_loop(socket, rx, standby_payload, running) {
            eprintln!("Network loop error: {}", e);
        }
    });

    if let Err(e) = video::run_video_receiver() {
        eprintln!("[Video] Forwarder failed: {}", e);
    }
    println!("\nShutting down.");
    Ok(())
}

/// This function's only job is to own the TCP stream and keep it alive.
/// It periodically tries to read, which will detect if the server closes the connection.
fn run_tcp_keepalive(mut stream: TcpStream, running: Arc<AtomicBool>) {
    stream
        .set_nonblocking(true)
        .expect("Failed to set TCP stream to non-blocking");
    let mut buf = [0; 16]; // Small buffer, we don't expect data

    println!("[TCP] Keep-alive thread started.");

    while running.load(Ordering::SeqCst) {
        match stream.read(&mut buf) {
            Ok(0) => {
                // Read 0 bytes, this means the server has closed the connection.
                println!("[TCP] Server closed the connection.");
                break;
            }
            Ok(_) => {
                // We received some unexpected data.
                println!("[TCP] Received unexpected data on control channel.");
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // This is the expected outcome in non-blocking mode.
                // It means the connection is still alive but there's no data to read.
            }
            Err(e) => {
                // A real error occurred.
                eprintln!("[TCP] Connection error: {}", e);
                break;
            }
        }
        thread::sleep(Duration::from_secs(1));
    }
    println!("[TCP] Keep-alive thread shutting down.");
}
