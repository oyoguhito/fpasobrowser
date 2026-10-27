//! Servo embedding PoC for the terminal renderer.
//!
//! This intentionally stays separate from the terminal BrowserClient. It
//! proves the first migration boundary: Servo loads a page,
//! paints into SoftwareRenderingContext, and the rendered pixels can be saved
//! as an ordinary PNG without an external browser executable.

use dpi::PhysicalSize;
use embedder_traits::EventLoopWaker;
use servo::{
    RenderingContext, ServoBuilder, SoftwareRenderingContext, WebView, WebViewBuilder,
    WebViewDelegate,
};
use std::{env, error::Error, rc::Rc, sync::atomic::AtomicBool, sync::Arc, thread, time::Duration};
use url::Url;

#[derive(Clone)]
struct Waker(Arc<AtomicBool>);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

struct Delegate;

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, webview: WebView) {
        webview.paint();
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("install rustls crypto provider");

    let mut args = env::args().skip(1);
    let url = Url::parse(args.next().as_deref().unwrap_or("https://example.com/"))?;
    let output = args
        .next()
        .unwrap_or_else(|| "servo-offscreen.png".to_owned());
    let size = PhysicalSize {
        width: 960,
        height: 540,
    };
    let rendering_context = Rc::new(
        SoftwareRenderingContext::new(size)
            .map_err(|error| format!("create Servo software context: {error:?}"))?,
    );
    rendering_context
        .make_current()
        .map_err(|error| format!("make Servo context current: {error:?}"))?;
    let servo = ServoBuilder::default()
        .event_loop_waker(Box::new(Waker(Arc::new(AtomicBool::new(false)))))
        .build();
    servo.setup_logging();
    let webview = WebViewBuilder::new(&servo, rendering_context.clone())
        .url(url)
        .delegate(Rc::new(Delegate))
        .build();

    // Servo is asynchronous. Keep spinning until a frame is available, with
    // a bounded wait so a broken network does not hang the PoC forever.
    let bounds = servo::DeviceIntRect::new(
        servo::DeviceIntPoint::zero(),
        servo::DeviceIntPoint::new(size.width as i32, size.height as i32),
    );
    for _ in 0..600 {
        servo.spin_event_loop();
        if let Some(image) = rendering_context.read_to_image(bounds) {
            image.save(&output)?;
            eprintln!("servo offscreen: wrote {output}");
            return Ok(());
        }
        thread::sleep(Duration::from_millis(16));
    }
    drop(webview);
    Err("Servo did not produce a frame within 10 seconds".into())
}
