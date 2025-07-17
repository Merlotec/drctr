// src/main.rs

pub mod handshake;
pub mod video;

use std::{
    io::{Read, Write},
    net::{TcpStream, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Context;

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

pub type ControlBits = [u8; 5];
const DEFAULT_CTRL_BITS: ControlBits = [0x80, 0x80, 0x80, 0x80, 0x00];
const CTRL_LAUNCH: u8 = 0x01;
const CTRL_LAND: u8 = 0x02;
const CTRL_AUX: u8 = 0x03;
const CTRL_PANIC: u8 = 0x04;

pub const fn create_heartbeat(ctrl_bits: ControlBits) -> Heartbeat {
    let verif_bit = ctrl_bits[0] ^ ctrl_bits[1] ^ ctrl_bits[2] ^ ctrl_bits[3] ^ ctrl_bits[4];
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

/// The same network-loop you had before.
fn run_network_loop(
    socket: UdpSocket,
    rx: Receiver<Command>,
    standby_payload: Heartbeat,
    running: Arc<AtomicBool>,
) -> std::io::Result<()> {
    let mut current_command: Option<ActiveCommand> = None;
    let mut small_hb_time = Instant::now();

    while running.load(Ordering::SeqCst) {
        // Receive new commands, respecting priority
        if let Ok(new_cmd) = rx.try_recv() {
            let override_ok = match &current_command {
                Some(active) => new_cmd.priority >= active.priority,
                None => true,
            };
            if override_ok {
                current_command = Some(ActiveCommand {
                    payload: new_cmd.payload,
                    end_time: Instant::now() + new_cmd.duration,
                    priority: new_cmd.priority,
                });
            }
        }

        // Expire old command
        if let Some(active) = &current_command {
            if Instant::now() >= active.end_time {
                current_command = None;
            }
        }

        // Send either active or standby
        if let Some(active) = &current_command {
            socket.send_to(&active.payload, config::DEST_ADDR)?;
        } else {
            socket.send_to(&standby_payload, config::DEST_ADDR)?;
        }

        // Small heartbeat once a second
        if small_hb_time.elapsed() >= Duration::from_secs(1) {
            socket.send_to(&[0x01, 0x01], config::DEST_ADDR)?;
            small_hb_time = Instant::now();
        }

        std::io::stdout().flush()?;
        thread::sleep(config::HEARTBEAT_INTERVAL);
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Channel for Commands
    let (tx, rx): (Sender<Command>, Receiver<Command>) = mpsc::channel();
    let standby_payload: Heartbeat = create_heartbeat(DEFAULT_CTRL_BITS);

    // Shared flag for shutdown
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    println!("Starting UDP packet sender…");
    println!("--> Sending to {}", config::DEST_ADDR);

    // Bind UDP & TCP for handshake
    let socket = UdpSocket::bind(config::SOURCE_ADDR)?;
    let mut tcp_stream =
        TcpStream::connect_timeout(&config::DEST_TCP_ADDR.parse()?, Duration::from_secs(5))?;
    tcp_stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    println!("[Handshake] TCP connected to {}", config::DEST_TCP_ADDR);

    // Spawn the network loop
    {
        let running_net = running.clone();
        thread::spawn(move || {
            if let Err(e) = run_network_loop(socket, rx, standby_payload, running_net) {
                eprintln!("[Network] Error: {}", e);
            }
        });
    }

    // Enter the SDL2 + video receiver loop, driving Command::send on keypress
    video::run_video_receiver(tx, running)?;

    println!("\nShutting down.");
    Ok(())
}
