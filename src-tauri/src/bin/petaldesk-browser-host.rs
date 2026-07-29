fn main() {
    if let Err(error) = petaldesk_lib::browser_native_host::run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
