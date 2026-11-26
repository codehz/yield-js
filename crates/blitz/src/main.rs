use std::process::ExitCode;

use anyrender_vello::VelloWindowRenderer;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("yield-blitz: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut session = yield_blitz::BlitzSession::new(yield_blitz::compile_fixture()?)?;
    session.run_until_idle()?;
    let event_loop = create_default_event_loop();
    let (proxy, receiver) = BlitzShellProxy::new(event_loop.create_proxy());
    let mut application = BlitzApplication::new(proxy, receiver);
    application.add_window(WindowConfig::new(
        Box::new(session),
        VelloWindowRenderer::new(),
    ));
    event_loop
        .run_app(application)
        .map_err(|error| error.to_string())
}
