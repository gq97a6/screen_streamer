use async_stream::stream;
use bytes::Bytes;
use image::{codecs::jpeg::JpegEncoder, DynamicImage, ImageBuffer, Rgba};
use scrap::{Capturer, Display};
use std::{convert::Infallible, thread, time::Duration};
use tokio::sync::broadcast;
use warp::Filter;

#[tokio::main]
async fn main() {
    let (frame_tx, _) = broadcast::channel::<Vec<u8>>(16);

    // Spawn a dedicated thread for capturing the screen.
    {
        let frame_tx = frame_tx.clone();
        thread::spawn(move || {
            let display = Display::primary().expect("Failed to get primary display.");
            let mut capturer = Capturer::new(display).expect("Failed to begin capture.");
            let (w, h) = (capturer.width(), capturer.height());
            println!("Capturing screen ({}x{})...", w, h);

            loop {
                match capturer.frame() {
                    Ok(frame) => {
                        if frame.len() != (w * h * 4) {
                            eprintln!("Unexpected frame size.");
                            continue;
                        }

                        // Convert BGRA to RGBA
                        let mut rgba_frame = Vec::with_capacity(w * h * 4);
                        for chunk in frame.chunks_exact(4) {
                            rgba_frame.extend_from_slice(&[chunk[2], chunk[1], chunk[0], chunk[3]]);
                        }

                        if let Some(img_buf) = ImageBuffer::<Rgba<u8>, _>::from_raw(w as u32, h as u32, rgba_frame) {
                            let dyn_img = DynamicImage::ImageRgba8(img_buf);
                            let mut jpeg_data = Vec::new();
                            {
                                let mut encoder = JpegEncoder::new(&mut jpeg_data);
                                if let Err(e) = encoder.encode_image(&dyn_img) {
                                    eprintln!("JPEG encode error: {:?}", e);
                                    continue;
                                }
                            }
                            let _ = frame_tx.send(jpeg_data);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(16)); // ~60 FPS
                    }
                    Err(e) => {
                        eprintln!("Error capturing frame: {:?}", e);
                        break;
                    }
                }
            }
        });
    }

    // Define the MJPEG streaming route.
    let stream_route = warp::path("stream")
        .and(warp::get())
        .map({
            let frame_tx = frame_tx.clone();
            move || {
                let mut rx = frame_tx.subscribe();
                let mjpeg_stream = stream! {
                    loop {
                        match rx.recv().await {
                            Ok(jpeg_data) => {
                                let header = format!(
                                    "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                                    jpeg_data.len()
                                );
                                yield Ok::<Bytes, Infallible>(Bytes::from(header));
                                yield Ok(Bytes::from(jpeg_data));
                                yield Ok(Bytes::from("\r\n"));
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                };

                let mut res = warp::reply::Response::new(warp::hyper::Body::wrap_stream(mjpeg_stream));
                res.headers_mut().insert(
                    "Content-Type",
                    "multipart/x-mixed-replace; boundary=frame".parse().unwrap(),
                );
                res
            }
        });

    // Serve a simple HTML page with an embedded video stream.
    let index_route = warp::path::end().map(|| {
        warp::reply::html(
            r#"
            <html>
              <head>
                <title>Screen Stream</title>
                <style>
                        html, body {
                            margin: 0;
                            height: 100%;
                        }

                        .image-container {
                            width: 100%;
                            height: 100vh; /* Full height of the viewport */
                            overflow: hidden; /* Hide overflow if necessary */
                        }

                        .responsive-image {
                            width: 100%;
                            height: 100%;
                            object-fit: contain; /* This maintains aspect ratio while filling the container */
                        }
                </style>
              </head>
              <body>
                <div class="image-container">
                    <img src="/stream" class="responsive-image">
                </div>
              </body>
            </html>
            "#,
        )
    });

    let routes = index_route.or(stream_route);

    println!("Server running at http://0.0.0.0:3030/");
    warp::serve(routes).run(([0, 0, 0, 0], 3030)).await;
}
