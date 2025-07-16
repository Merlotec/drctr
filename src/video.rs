use ::anyhow::Context;
use ffmpeg_next as ffmpeg;
use ffmpeg_next::{
    Dictionary, codec::Context as CodecContext, format::input_with_dictionary, media::Type,
};
use sdl2::{event::Event, pixels::PixelFormatEnum};
use std::{
    sync::mpsc::{self, TryRecvError},
    thread,
    time::Duration,
};

pub fn run_video_receiver() -> anyhow::Result<()> {
    // 1) Initialize FFmpeg
    ffmpeg::init().context("Failed to init ffmpeg")?;
    // 3) Channel to send decoded frames to the main (SDL) thread
    let (tx, rx) = mpsc::sync_channel::<DecodedFrame>(1);

    // 4) Spawn a thread to grab & decode frames
    thread::spawn(move || {
        let mut opts = Dictionary::new();

        // 2) Build RTSP‐over‐UDP options
        opts.set("rtsp_transport", "udp"); // UDP for media
        opts.set("stimeout", "5000000"); // 5s I/O timeout in µs
        opts.set("err_detect", "explode"); // strict error detection

        if let Err(e) = decode_thread(tx, opts) {
            eprintln!("Decoder thread error: {:?}", e);
        }
    });

    // 5) Initialize SDL2 and create a window + texture
    let sdl = sdl2::init().expect("Failed to init SDL2");
    let video_subsystem = sdl.video().expect("Failed to init SDL video");
    // We size the window once we get the first frame’s dimensions:
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

    // 6) Main loop: wait for frames and handle SDL events
    let mut texture = None;
    'running: loop {
        // Handle quit event
        for ev in event_pump.poll_iter() {
            if let Event::Quit { .. } = ev {
                break 'running;
            }
        }

        // Try to receive a decoded frame
        match rx.try_recv() {
            Ok(frame) => {
                // On first frame, create the texture
                if texture.is_none() {
                    let tex = texture_creator
                        .create_texture_streaming(PixelFormatEnum::IYUV, frame.width, frame.height)
                        .expect("Failed to create texture");
                    texture = Some(tex);
                    canvas.window_mut().set_size(frame.height, frame.width).ok();
                }
                if let Some(tex) = &mut texture {
                    // Update YUV planes
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

                    // Render
                    canvas.clear();
                    canvas
                        .copy_ex(tex, None, None, 90.0, None, false, false)
                        .expect("Failed to copy texture");
                    canvas.present();
                }
            }
            Err(TryRecvError::Empty) => {
                // no new frame yet
                thread::sleep(Duration::from_millis(5));
            }
            Err(TryRecvError::Disconnected) => {
                eprintln!("Decoder thread disconnected");
                break 'running;
            }
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

/// Runs in a background thread: opens the RTSP feed, decodes frames,
/// and sends them over the channel.
fn decode_thread(
    tx: mpsc::SyncSender<DecodedFrame>,
    opts: Dictionary<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Open RTSP URI
    let uri = "rtsp://192.168.1.1:7070/webcam";
    let mut ictx = input_with_dictionary(uri, opts)?;

    // Find video stream
    let input = ictx.streams().best(Type::Video).ok_or("No video stream")?;
    let stream_index = input.index();

    // Open software decoder
    let mut decoder = CodecContext::from_parameters(input.parameters())?
        .decoder()
        .video()?; // Packet loop

    for (stream, packet) in ictx.packets() {
        if stream.index() != stream_index {
            continue;
        }
        decoder.send_packet(&packet)?;
        let mut frame = ffmpeg::util::frame::video::Video::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
            // Extract YUV planes
            let (w, h) = (frame.width(), frame.height());
            let y_stride = frame.stride(0);
            let uv_stride = frame.stride(1);
            let y_size = (y_stride as i32 * h as i32) as usize;
            let uv_size = (uv_stride as i32 * (h as i32 / 2)) as usize;

            let mut y_plane = vec![0u8; y_size];
            let mut u_plane = vec![0u8; uv_size];
            let mut v_plane = vec![0u8; uv_size];
            // copy row by row (stride may exceed width)
            for row in 0..h as usize {
                let src =
                    &frame.data(0)[row * y_stride as usize..row * y_stride as usize + w as usize];
                let dst =
                    &mut y_plane[row * y_stride as usize..row * y_stride as usize + w as usize];
                dst.copy_from_slice(src);
            }
            for row in 0..(h as usize / 2) {
                let src_u = &frame.data(1)
                    [row * uv_stride as usize..row * uv_stride as usize + (w as usize / 2)];
                let dst_u = &mut u_plane
                    [row * uv_stride as usize..row * uv_stride as usize + (w as usize / 2)];
                dst_u.copy_from_slice(src_u);
                let src_v = &frame.data(2)
                    [row * uv_stride as usize..row * uv_stride as usize + (w as usize / 2)];
                let dst_v = &mut v_plane
                    [row * uv_stride as usize..row * uv_stride as usize + (w as usize / 2)];
                dst_v.copy_from_slice(src_v);
            }

            // Send to main thread (drop old frame if still unconsumed)
            let _ = tx.try_send(DecodedFrame {
                width: w,
                height: h,
                y_stride,
                uv_stride,
                y_plane,
                u_plane,
                v_plane,
            });
        }
    }
    Ok(())
}
