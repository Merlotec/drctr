// src/video.rs

use anyhow::Context;
use ffmpeg_next as ffmpeg;
use ffmpeg_next::{
    Dictionary, codec::Context as CodecContext, format::input_with_dictionary, media::Type,
};
use sdl2::{event::Event, keyboard::Scancode, pixels::PixelFormatEnum};
use std::{
    sync::{
        Arc,
        atomic::AtomicBool,
        mpsc::{Receiver, Sender},
    },
    thread,
    time::Duration,
};

use crate::{Command, ControlBits, DEFAULT_CTRL_BITS, Heartbeat, config, create_heartbeat};

pub fn run_video_receiver(tx_cmd: Sender<Command>, running: Arc<AtomicBool>) -> anyhow::Result<()> {
    // 1) Initialize FFmpeg
    ffmpeg::init().context("Failed to init ffmpeg")?;

    // 3) Channel for decoded frames
    let (tx, rx) = std::sync::mpsc::sync_channel::<DecodedFrame>(1);

    // 4) Spawn decode thread (unchanged)
    thread::spawn(move || {
        let mut opts = Dictionary::new();
        opts.set("rtsp_transport", "udp");
        opts.set("stimeout", "5000000");
        opts.set("err_detect", "explode");
        if let Err(e) = decode_thread(tx, opts) {
            eprintln!("Decoder thread error: {:?}", e);
        }
    });

    // 5) Initialize SDL2
    let sdl = sdl2::init().expect("Failed to init SDL2");
    let video_subsystem = sdl.video().expect("Failed to init SDL video");
    let window = video_subsystem
        .window("RTSP UDP Player", 800, 600)
        .position_centered()
        .resizable()
        .build()
        .context("Failed to create window")?;
    let mut canvas = window
        .into_canvas()
        .accelerated()
        .build()
        .context("Failed to create canvas")?;
    let texture_creator = canvas.texture_creator();
    let mut event_pump = sdl.event_pump().expect("Failed to get SDL event pump");

    let mut texture = None;
    let mut width = 0;
    let mut height = 0;

    // Main loop
    while running.load(std::sync::atomic::Ordering::SeqCst) {
        // 6a) Handle SDL events (window + keyboard)
        for ev in event_pump.poll_iter() {
            match ev {
                Event::Quit { .. } => running.store(false, std::sync::atomic::Ordering::SeqCst),
                Event::KeyDown {
                    scancode: Some(sc), ..
                } => {
                    let mut ctrl_bits: ControlBits = DEFAULT_CTRL_BITS;
                    match sc {
                        Scancode::Space => ctrl_bits[4] = crate::CTRL_LAUNCH,
                        Scancode::L => ctrl_bits[4] = crate::CTRL_LAND,
                        Scancode::O => ctrl_bits[4] = crate::CTRL_AUX,
                        Scancode::P => ctrl_bits[4] = crate::CTRL_PANIC,
                        Scancode::A | Scancode::Left => {
                            ctrl_bits[0] = ctrl_bits[0].saturating_sub(0x1d)
                        }
                        Scancode::D | Scancode::Right => {
                            ctrl_bits[0] = ctrl_bits[0].saturating_add(0x1d)
                        }
                        Scancode::W | Scancode::Up => {
                            ctrl_bits[1] = ctrl_bits[1].saturating_add(0x1d)
                        }
                        Scancode::S | Scancode::Down => {
                            ctrl_bits[1] = ctrl_bits[1].saturating_sub(0x1d)
                        }
                        Scancode::Q => ctrl_bits[3] = ctrl_bits[3].saturating_add(0x1d),
                        Scancode::E => ctrl_bits[3] = ctrl_bits[3].saturating_sub(0x1d),
                        _ => {}
                    }
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
                        let _ = tx_cmd.send(Command {
                            payload: packet,
                            duration: config::MOVEMENT_COMMAND_DUR,
                            priority: 0,
                        });
                    }
                }
                _ => {}
            }
        }

        // 6b) Handle decoded frames
        match rx.try_recv() {
            Ok(frame) => {
                if texture.is_none() {
                    width = frame.width;
                    height = frame.height;
                    let tex = texture_creator
                        .create_texture_streaming(PixelFormatEnum::IYUV, width, height)
                        .expect("Failed to create texture");
                    texture = Some(tex);
                    canvas.window_mut().set_size(width, height).ok();
                }
                if let Some(tex) = &mut texture {
                    tex.update_yuv(
                        None,
                        &frame.y_plane,
                        frame.y_stride as usize,
                        &frame.u_plane,
                        frame.uv_stride as usize,
                        &frame.v_plane,
                        frame.uv_stride as usize,
                    )
                    .expect("Failed to update texture");

                    canvas.clear();
                    canvas
                        .copy_ex(tex, None, None, 90.0, None, false, false)
                        .expect("Failed to copy texture");
                    canvas.present();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
    }

    Ok(())
}

/// Holds one decoded YUV420p frame.
struct DecodedFrame {
    width: u32,
    height: u32,
    y_stride: usize,
    uv_stride: usize,
    y_plane: Vec<u8>,
    u_plane: Vec<u8>,
    v_plane: Vec<u8>,
}

/// Runs in a background thread: opens RTSP, decodes frames, sends them over the channel.
fn decode_thread(
    tx: std::sync::mpsc::SyncSender<DecodedFrame>,
    opts: Dictionary<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let uri = "rtsp://192.168.1.1:7070/webcam";
    let mut ictx = input_with_dictionary(uri, opts)?;

    let input = ictx.streams().best(Type::Video).ok_or("No video stream")?;
    let stream_index = input.index();

    let mut decoder = CodecContext::from_parameters(input.parameters())?
        .decoder()
        .video()?;

    for (stream, packet) in ictx.packets() {
        if stream.index() != stream_index {
            continue;
        }
        decoder.send_packet(&packet)?;
        let mut frame = ffmpeg::util::frame::video::Video::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
            let (w, h) = (frame.width(), frame.height());
            let y_stride = frame.stride(0);
            let uv_stride = frame.stride(1);
            let y_size = (y_stride as i32 * h as i32) as usize;
            let uv_size = (uv_stride as i32 * (h as i32 / 2)) as usize;

            let mut y_plane = vec![0u8; y_size];
            let mut u_plane = vec![0u8; uv_size];
            let mut v_plane = vec![0u8; uv_size];

            for row in 0..h as usize {
                let src = &frame.data(0)[row * y_stride as usize..][..w as usize];
                let dst = &mut y_plane[row * y_stride as usize..][..w as usize];
                dst.copy_from_slice(src);
            }
            for row in 0..(h as usize / 2) {
                let src_u = &frame.data(1)[row * uv_stride as usize..][..(w as usize / 2)];
                let dst_u = &mut u_plane[row * uv_stride as usize..][..(w as usize / 2)];
                dst_u.copy_from_slice(src_u);
                let src_v = &frame.data(2)[row * uv_stride as usize..][..(w as usize / 2)];
                let dst_v = &mut v_plane[row * uv_stride as usize..][..(w as usize / 2)];
                dst_v.copy_from_slice(src_v);
            }

            let _ = tx.try_send(DecodedFrame {
                width: w,
                height: h,
                y_stride: y_stride as usize,
                uv_stride: uv_stride as usize,
                y_plane,
                u_plane,
                v_plane,
            });
        }
    }
    Ok(())
}
