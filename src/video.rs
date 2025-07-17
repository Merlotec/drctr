// src/video.rs

use anyhow::Context;
use ffmpeg_next as ffmpeg;
use ffmpeg_next::{
    Dictionary, codec::Context as CodecContext, format::input_with_dictionary, media::Type,
};
use std::sync::mpsc::SyncSender;

/// Holds one decoded YUV420p frame.
/// This struct is made public so the `ui` module can use it.
#[derive(Debug)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub y_stride: usize,
    pub uv_stride: usize,
    pub y_plane: Vec<u8>,
    pub u_plane: Vec<u8>,
    pub v_plane: Vec<u8>,
}

/// Initializes FFmpeg and spawns the decoding thread.
/// It takes a SyncSender to send decoded frames to the UI thread.
pub fn run_video_decoder(tx_frame: SyncSender<DecodedFrame>) -> anyhow::Result<()> {
    // 1) Initialize FFmpeg
    ffmpeg::init().context("Failed to init ffmpeg")?;

    // 2) Spawn the decode thread, moving the sender into it.
    std::thread::spawn(move || {
        let mut opts = Dictionary::new();
        opts.set("rtsp_transport", "udp");
        opts.set("stimeout", "5000000"); // 5 seconds
        opts.set("err_detect", "explode");
        if let Err(e) = decode_thread(tx_frame, opts) {
            eprintln!("Decoder thread encountered an error: {:?}", e);
        }
    });

    Ok(())
}

/// Runs in a background thread: opens the RTSP stream, decodes frames,
/// and sends them over the channel.
fn decode_thread(
    tx: SyncSender<DecodedFrame>,
    opts: Dictionary<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Connect to the RTSP stream
    let uri = "rtsp://192.168.1.1:7070/webcam";
    let mut ictx = input_with_dictionary(uri, opts)?;

    // Find the best video stream
    let input = ictx
        .streams()
        .best(Type::Video)
        .ok_or("No video stream found in the input")?;
    let stream_index = input.index();

    // Create a decoder for the video stream
    let mut decoder = CodecContext::from_parameters(input.parameters())?
        .decoder()
        .video()?;

    // Create an empty frame to receive decoded data
    let mut frame = ffmpeg::util::frame::video::Video::empty();

    // Loop through all packets in the stream
    for (stream, packet) in ictx.packets() {
        // Process only packets from our target video stream
        if stream.index() != stream_index {
            continue;
        }

        // Send the packet to the decoder
        decoder.send_packet(&packet)?;

        // Receive all available frames from the decoder
        while decoder.receive_frame(&mut frame).is_ok() {
            // Clone the frame data into our custom struct.
            // This is necessary to send it to another thread.
            let decoded_frame = DecodedFrame {
                width: frame.width(),
                height: frame.height(),
                y_stride: frame.stride(0) as usize,
                uv_stride: frame.stride(1) as usize,
                y_plane: frame.data(0).to_vec(),
                u_plane: frame.data(1).to_vec(),
                v_plane: frame.data(2).to_vec(),
            };

            // Use try_send for non-blocking behavior. If the UI thread is lagging
            // and the channel is full, it will drop the frame instead of blocking
            // the decoding thread.
            if tx.try_send(decoded_frame).is_err() {
                // This is not a critical error, just means the UI can't keep up.
                // You could add logging here if needed.
            }
        }
    }

    println!("Decoder thread finished.");
    Ok(())
}
