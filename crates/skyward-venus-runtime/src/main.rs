fn main() {
    if let Err(error) = skyward_venus_runtime::run(std::env::args_os().collect()) {
        eprintln!("venus-runtime: {error:#}");
        std::process::exit(1);
    }
}
