fn main() {
    let mut args = vec![
        std::ffi::OsString::from("venus-runtime"),
        std::ffi::OsString::from("network-suspend-probe"),
    ];
    args.extend(std::env::args_os().skip(1));
    if let Err(error) = skyward_venus_runtime::run(args) {
        eprintln!("network-suspend-probe: {error:#}");
        std::process::exit(1);
    }
}
