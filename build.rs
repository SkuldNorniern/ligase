use std::env;

use winres::WindowsResource;

fn main() {
    // the exe carries the app icon, so explorer and the taskbar show it
    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = WindowsResource::new();
        res.set_icon("assets/icons/app/ligase.ico");
        if let Err(e) = res.compile() {
            panic!("winres: {e}");
        }
    }
}
