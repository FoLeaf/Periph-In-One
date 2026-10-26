//! Disposable executables for OTA experiments. Never run against an installation.

pub fn run(version: &str) {
    use std::io::Write;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.first().and_then(|a| a.to_str()) {
        Some("--version") => println!("{version}"),
        Some("--hold") => {
            println!("ready");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        // Deliberately failing input, only in a disposable copied fixture.
        Some("--self-replace-missing") => {
            let exe = std::env::current_exe().unwrap();
            let missing = exe.parent().unwrap().join("missing-candidate.exe");
            assert!(!missing.exists());
            let result = self_replace::self_replace(&missing);
            println!("failed={} canonical={}", result.is_err(), exe.exists());
        }
        _ => std::process::exit(2),
    }
}
