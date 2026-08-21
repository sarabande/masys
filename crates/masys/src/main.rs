fn main() -> std::process::ExitCode {
    match masys::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            // Printing to stderr and exiting non-zero rather than returning
            // `Result` from `main`, which would print the error's `Debug`
            // form and still exit 1 - neither readable nor scriptable.
            eprintln!("masys: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}
