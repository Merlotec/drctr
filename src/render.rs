// src/ui.rs (or src/render.rs)

use crate::{
    Command, ControlBits, DEFAULT_CTRL_BITS, Heartbeat, config, create_heartbeat,
    video::DecodedFrame,
};
use anyhow::Context;
use sdl2::{
    EventPump,
    event::Event,
    keyboard::Scancode,
    pixels::PixelFormatEnum,
    render::{Canvas, Texture},
    video::Window,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender},
    },
    thread,
    time::Duration,
};

/// Converts a keyboard scancode into a `Command`.
/// This is now a free function to avoid borrow-checker conflicts with `event_pump`.
fn scancode_to_command(sc: Scancode) -> Option<Command> {
    let mut ctrl_bits: ControlBits = DEFAULT_CTRL_BITS;
    match sc {
        Scancode::Space => ctrl_bits[4] = crate::CTRL_LAUNCH,
        Scancode::L => ctrl_bits[4] = crate::CTRL_LAND,
        Scancode::O => ctrl_bits[4] = crate::CTRL_AUX,
        Scancode::P => ctrl_bits[4] = crate::CTRL_PANIC,
        Scancode::A | Scancode::Left => ctrl_bits[0] = ctrl_bits[0].saturating_sub(0x1d),
        Scancode::D | Scancode::Right => ctrl_bits[0] = ctrl_bits[0].saturating_add(0x1d),
        Scancode::W | Scancode::Up => ctrl_bits[1] = ctrl_bits[1].saturating_add(0x1d),
        Scancode::S | Scancode::Down => ctrl_bits[1] = ctrl_bits[1].saturating_sub(0x1d),
        Scancode::Q => ctrl_bits[3] = ctrl_bits[3].saturating_add(0x1d),
        Scancode::E => ctrl_bits[3] = ctrl_bits[3].saturating_sub(0x1d),
        _ => return None, // Not a recognized command key
    }

    // Only create a command if the control bits have actually changed
    if ctrl_bits != DEFAULT_CTRL_BITS {
        let packet: Heartbeat = create_heartbeat(ctrl_bits);
        println!(
            "[Input] {}",
            packet
                .iter()
                .map(|b| format!("{:02X}", b))
                .collect::<Vec<_>>()
                .join(" ")
        );
        return Some(Command {
            payload: packet,
            duration: config::MOVEMENT_COMMAND_DUR,
            priority: 0,
        });
    }
    None
}

/// The UI struct is simplified to hold only the core SDL components.
/// State related to the texture is now managed within the `run` method
/// to avoid self-referential lifetime issues.
pub struct UI {
    canvas: Canvas<Window>,
    event_pump: EventPump,
}

impl UI {
    /// Initializes SDL, creates the window and canvas.
    pub fn new() -> anyhow::Result<Self> {
        let sdl_context = sdl2::init().map_err(|s| anyhow::anyhow!(s))?;
        let video_subsystem = sdl_context.video().map_err(|s| anyhow::anyhow!(s))?;
        let window = video_subsystem
            .window("RTSP UDP Player", 800, 600)
            .position_centered()
            .resizable()
            .build()
            .context("Failed to create window")?;

        let canvas = window
            .into_canvas()
            .accelerated()
            .build()
            .context("Failed to create canvas")?;

        let event_pump = sdl_context.event_pump().map_err(|s| anyhow::anyhow!(s))?;

        Ok(Self { canvas, event_pump })
    }

    /// Runs the main UI loop.
    /// This function will take ownership of `self` and run until the application quits.
    pub fn run(
        mut self,
        rx_frame: Receiver<DecodedFrame>,
        tx_cmd: Sender<Command>,
        running: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        let texture_creator = self.canvas.texture_creator();
        let mut texture: Option<Texture> = None;
        let mut width = 0;
        let mut height = 0;

        while running.load(Ordering::SeqCst) {
            // 1. Handle keyboard and window events
            for ev in self.event_pump.poll_iter() {
                match ev {
                    Event::Quit { .. } => running.store(false, Ordering::SeqCst),
                    Event::KeyDown {
                        scancode: Some(sc), ..
                    } => {
                        // Call the free function, which solves the borrow error
                        if let Some(command) = scancode_to_command(sc) {
                            if tx_cmd.send(command).is_err() {
                                eprintln!("Failed to send command: main thread may have exited.");
                            }
                        }
                    }
                    _ => {}
                }
            }

            // 2. Check for and handle a new frame from the decoder
            if let Ok(frame) = rx_frame.try_recv() {
                // Create or resize texture if necessary
                if texture.is_none() || width != frame.width || height != frame.height {
                    width = frame.width;
                    height = frame.height;
                    let tex = texture_creator
                        .create_texture_streaming(PixelFormatEnum::IYUV, width, height)
                        .map_err(|e| anyhow::anyhow!("Failed to create texture: {}", e))?;
                    texture = Some(tex);
                    self.canvas.window_mut().set_size(width, height).ok();
                }

                // Update the texture with the new frame data
                if let Some(tex) = &mut texture {
                    tex.update_yuv(
                        None,
                        &frame.y_plane,
                        frame.y_stride,
                        &frame.u_plane,
                        frame.uv_stride,
                        &frame.v_plane,
                        frame.uv_stride,
                    )
                    .map_err(|e| anyhow::anyhow!("Failed to update texture: {}", e))?;
                }
            }

            // 3. Render the current texture to the screen
            if let Some(tex) = &texture {
                self.canvas.clear();
                self.canvas
                    .copy_ex(tex, None, None, 90.0, None, false, false)
                    .map_err(|e| anyhow::anyhow!("Failed to copy texture to canvas: {}", e))?;
                self.canvas.present();
            }

            // Sleep briefly to prevent the loop from using 100% CPU
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
}
